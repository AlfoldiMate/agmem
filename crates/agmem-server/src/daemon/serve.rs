//! The process that owns the store.
//!
//! It holds the data-dir lock for its whole life, opens the store and the
//! embedder once, and then serves one [`AgmemService`] per attached session
//! over a Unix socket. Sessions come and go; the store does not.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use agmem_embed::Embedder;
use agmem_store::db::Db;
use anyhow::Context;
use rmcp::ServiceExt;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::task::JoinSet;
use tokio::time::Instant;

use crate::config::Config;
use crate::daemon::{Ack, Handshake, Refusal, socket_path};
use crate::service::AgmemService;
use crate::{doctor, embedder, lock};

/// How long a retiring daemon keeps serving the sessions already attached
/// before it exits (issue #112). Long enough for a tool call in flight to
/// answer; short enough that the upgrade someone just ran does not look like
/// a hang. The session that retired it is waiting on the store lock this
/// process holds, so every second here is a second before the new daemon
/// can start.
pub const DRAIN: Duration = Duration::from_secs(2);

/// How long a daemon on its way out waits for the engine to close the store
/// and release the store's own file lock, before it gives up its data-dir
/// lock regardless (issue #124).
const STORE_RELEASE_DEADLINE: Duration = Duration::from_secs(10);

/// Own the store and serve every session that attaches, until nothing has
/// been attached for `idle_timeout` — or a newer release attaches, in which
/// case this daemon retires so that release can serve.
///
/// On the way out it waits for the engine to let go of the store before it
/// releases the data-dir lock (issue #124): the session that retired it is
/// polling that lock, and starts a daemon the moment it is free.
///
/// # Errors
/// When the lock is already held, the store will not open or migrate, the
/// embedder will not load, or the socket cannot be bound. The `--daemon-serve`
/// process writes the error to its log as well as returning it: its stderr
/// is closed, and the log is the only place a detached process has.
pub async fn run(cfg: Config) -> anyhow::Result<()> {
    let outcome = serve(cfg).await;
    if let Err(error) = &outcome {
        tracing::error!(error = %format!("{error:#}"), "the shared store stopped with an error");
    }
    outcome
}

async fn serve(cfg: Config) -> anyhow::Result<()> {
    let path = socket_path(&cfg.data_dir)?;
    // The lock is what makes "one owner" true; the socket only advertises it.
    // Holding it here is also what makes the unlink below safe — no other
    // daemon can be alive to have the socket we are about to replace.
    let _lock = lock::acquire(&cfg.data_dir)?;
    restrict(&cfg.data_dir);

    let db = agmem_store::db::connect_with(&cfg.db_url, cfg.db_credentials()).await?;
    let schema = agmem_store::migrate::ensure(&db).await?;
    let embedder = embedder::build(&cfg)?;
    agmem_store::migrate::ensure_embedder(&db, embedder.model_id(), embedder.dim()).await?;
    // A daemon is where the sweep belongs: it is the process a machine starts
    // once, and the sessions attaching to it never start anything.
    let pruned = crate::startup::prune(&db).await;
    // The checks `--doctor` cannot run while a daemon holds the store, run by
    // the process that holds it (issue #112). They go to the log, not to the
    // exit code: the schema and the embedder are up, so the store can serve,
    // and a scratch write that failed is a line to read in daemon.log rather
    // than a reason to leave every session without memory.
    let checks = doctor::selfcheck(&db, embedder.as_ref()).await;
    for check in &checks {
        match &check.outcome {
            Ok(detail) => tracing::info!(check = check.name, %detail, "selfcheck ok"),
            Err(error) => {
                tracing::warn!(check = check.name, %error, "selfcheck failed; serving anyway")
            }
        }
    }

    // Bind last, and only once everything above has worked. A daemon that
    // advertises itself and then dies on migrate would invite every session
    // that finds the socket to respawn it, forever.
    let _ = std::fs::remove_file(&path);
    let listener =
        UnixListener::bind(&path).with_context(|| format!("cannot bind {}", path.display()))?;
    tracing::info!(
        schema,
        embedder = embedder.model_id(),
        dim = embedder.dim(),
        pruned,
        checks = checks.len(),
        socket = %path.display(),
        "shared store ready"
    );
    if cfg.took_over {
        // The sessions that were on the old daemon are pumping a closed
        // socket: their memory tools are gone until they start again. This
        // is the one place that fact is written down.
        tracing::info!(
            release = crate::daemon::RELEASE,
            "took over from a retired daemon; sessions still attached to the old one need a restart"
        );
    }

    let idle = Duration::from_secs(cfg.idle_timeout);
    let daemon = Arc::new(cfg);
    // Once set, every handshake still in flight is answered "retiring" rather
    // than "ok": a session accepted onto a daemon that is about to exit would
    // come up with memory tools and lose them a moment later.
    let retiring = Arc::new(AtomicBool::new(false));
    let mut sessions = JoinSet::<SessionEnd>::new();
    let mut listener = Some(listener);
    let mut drain_until: Option<Instant> = None;

    loop {
        // Read once per turn: the branches below hold `sessions` borrowed
        // while they wait, so none of them can ask it again.
        let attached = sessions.len();
        tokio::select! {
            accepted = accept_on(listener.as_ref()), if listener.is_some() => {
                let (stream, _) = accepted.context("accepting on the agmem socket")?;
                let (db, embedder, daemon, retiring) =
                    (db.clone(), Arc::clone(&embedder), Arc::clone(&daemon), Arc::clone(&retiring));
                sessions.spawn(async move {
                    match session(stream, db, embedder, &daemon, &retiring).await {
                        Ok(end) => end,
                        Err(error) => {
                            tracing::warn!(error = %format!("{error:#}"), "session ended badly");
                            SessionEnd::Detached
                        }
                    }
                });
            }
            Some(ended) = sessions.join_next(), if attached > 0 => {
                let end = ended.unwrap_or_else(|error| {
                    tracing::warn!(%error, "a session task did not finish");
                    SessionEnd::Detached
                });
                let attached = attached.saturating_sub(1);
                match end {
                    SessionEnd::Retiring if drain_until.is_none() => {
                        // Sessions may still be attached, but they are attached
                        // to code from a release that is no longer on disk;
                        // cutting them loose is the fix, not the damage
                        // (issue #60 — a stale daemon otherwise serves old
                        // schema and scoring until its idle timeout). They get
                        // `DRAIN` to answer whatever is in flight, no more.
                        retiring.store(true, Ordering::SeqCst);
                        listener = None;
                        // Unlink now, not at exit: the refused session is
                        // waiting for exactly this before it starts a daemon
                        // from its binary, and every second of the drain
                        // would otherwise be added to its wait.
                        let _ = std::fs::remove_file(&path);
                        drain_until = Some(Instant::now() + DRAIN);
                        tracing::warn!(
                            attached,
                            drain = ?DRAIN,
                            "a newer release attached; retiring so its binary can serve — \
                             sessions still attached need a restart"
                        );
                        if attached == 0 {
                            break;
                        }
                    }
                    SessionEnd::Retiring | SessionEnd::Detached if drain_until.is_some() => {
                        if attached == 0 {
                            tracing::info!("every session detached; retiring now");
                            break;
                        }
                    }
                    SessionEnd::Retiring | SessionEnd::Detached => {
                        tracing::debug!(attached, "session detached");
                    }
                }
            }
            () = tokio::time::sleep_until(drain_until.unwrap_or_else(Instant::now)), if drain_until.is_some() => {
                tracing::warn!(attached, "drain window over; cutting the remaining sessions loose");
                break;
            }
            () = idle_elapsed(idle, attached == 0), if drain_until.is_none() => {
                tracing::info!(?idle, "nothing attached; shutting down");
                break;
            }
        }
    }

    // In the binary the process exit does this; in a test the daemon is a
    // task, and its sessions must not outlive it. Aborting is a request: the
    // tasks, and the store handles they hold, go when the runtime gets to
    // them, and the handle dropped below has to be the last one.
    sessions.abort_all();
    while sessions.join_next().await.is_some() {}
    drop(listener);
    if drain_until.is_none() {
        // A retiring daemon unlinked its socket when it stopped accepting,
        // and the path may already belong to the daemon that replaced it.
        let _ = std::fs::remove_file(&path);
    }
    // With the last handle gone the engine shuts the store down on a task of
    // its own, and the store's file lock is released at the end of that —
    // some milliseconds from now, while the data-dir lock `_lock` above is
    // released the moment this returns. A session polling the data-dir lock
    // would start a daemon in that gap, and that daemon would die on the
    // store (issue #124). So: wait for the store to let go first.
    drop(db);
    wait_for_store_release(&daemon.db_url).await;
    Ok(())
}

/// Wait until the engine has released the store's own file lock, or
/// [`STORE_RELEASE_DEADLINE`] has passed.
///
/// The probe is a second descriptor on the lock file: `flock` conflicts
/// between descriptors of one process too, so it succeeds exactly when the
/// engine's descriptor has gone. Engines that keep no lock file this process
/// can see — `mem://`, a remote server — have nothing to wait for.
async fn wait_for_store_release(db_url: &str) {
    let Some(path) = store_lock_path(db_url) else {
        return;
    };
    let started = Instant::now();
    loop {
        let probe = std::fs::File::options()
            .read(true)
            .write(true)
            .open(&path)
            .map(|file| file.try_lock().map(|()| drop(file)));
        match probe {
            // No lock file: the engine never took one, or the store is gone.
            Err(_) | Ok(Ok(())) => {
                tracing::info!(elapsed = ?started.elapsed(), "the store let go");
                return;
            }
            Ok(Err(std::fs::TryLockError::WouldBlock)) => {}
            Ok(Err(std::fs::TryLockError::Error(error))) => {
                tracing::warn!(%error, path = %path.display(), "cannot probe the store's lock; exiting anyway");
                return;
            }
        }
        if started.elapsed() >= STORE_RELEASE_DEADLINE {
            tracing::warn!(
                deadline = ?STORE_RELEASE_DEADLINE,
                "the store did not let go in time; exiting anyway"
            );
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// The lock file an embedded surrealkv store keeps at its root, for the
/// engines that keep one.
fn store_lock_path(db_url: &str) -> Option<std::path::PathBuf> {
    db_url
        .strip_prefix("surrealkv://")
        .map(|root| std::path::Path::new(root).join("LOCK"))
}

/// The next connection on `listener`; parks when there is no listener. The
/// `select!` guard keeps this from being polled in that state, but a branch
/// still has to be a future.
async fn accept_on(
    listener: Option<&UnixListener>,
) -> std::io::Result<(UnixStream, tokio::net::unix::SocketAddr)> {
    match listener {
        Some(listener) => listener.accept().await,
        None => std::future::pending().await,
    }
}

/// Completes once `idle` has passed with nothing attached.
///
/// A `select!` branch that never finishes is how "not this time" is spelled,
/// so an idle timeout of zero — keep the daemon forever — and a daemon with
/// sessions on it both park here.
async fn idle_elapsed(idle: Duration, nothing_attached: bool) {
    if idle.is_zero() || !nothing_attached {
        std::future::pending::<()>().await;
    }
    tokio::time::sleep(idle).await;
}

/// How one attached session ended, as far as the accept loop cares.
enum SessionEnd {
    /// The ordinary way: the client went away, the daemon keeps serving.
    Detached,
    /// The session ran a newer release; the daemon refused it and now shuts
    /// down so that session can start a daemon from its own binary.
    Retiring,
}

/// One attached session: find out who is asking, answer with an [`Ack`],
/// then hand rmcp the socket.
async fn session(
    stream: UnixStream,
    db: Db,
    embedder: Arc<dyn Embedder>,
    daemon: &Config,
    retiring: &AtomicBool,
) -> anyhow::Result<SessionEnd> {
    let (read, mut write) = stream.into_split();
    let mut read = BufReader::new(read);
    let mut line = String::new();
    let bytes = read
        .read_line(&mut line)
        .await
        .context("reading the session handshake")?;
    if bytes == 0 {
        // Connect-and-leave is how `--doctor` asks whether a daemon is here.
        tracing::debug!("a probe attached and left without a handshake");
        return Ok(SessionEnd::Detached);
    }
    let asked: Handshake = serde_json::from_str(line.trim())
        .context("the session handshake is not the JSON this daemon expects")?;

    // The ack goes over the socket either way (issue #60): a refusal that
    // only lands in daemon.log is one the session pumps through as EOF and
    // exits 0 on — no memory tools, no explanation.
    let decision = if retiring.load(Ordering::SeqCst) {
        // Accepted after another session retired this daemon: the answer is
        // the same "wait for the fresh one" that session got, not an `ok`
        // on a socket about to close (issue #112).
        Err(Refusal::already_retiring())
    } else {
        Handshake::new(daemon).accept(&asked)
    };
    let ack = match &decision {
        Ok(()) => Ack::accepted(),
        Err(refusal) => Ack::refused(refusal),
    };
    let mut ack_line = serde_json::to_vec(&ack)?;
    ack_line.push(b'\n');
    write
        .write_all(&ack_line)
        .await
        .context("writing the handshake ack")?;
    if let Err(refusal) = decision {
        if refusal.retire {
            tracing::warn!(%refusal, release = %asked.release, "refused a session; retiring");
            return Ok(SessionEnd::Retiring);
        }
        return Err(refusal.into());
    }

    // `main.rs` does this at startup for the in-process path (design §5.1
    // step 8). Here the connection is the startup: the daemon has never heard
    // of a project until one attaches, and a space missing from the registry
    // is silently left out of `space: "all"`.
    agmem_store::repo::ensure_space(&db, &asked.space).await?;
    tracing::info!(space = %asked.space, "session attached");

    let session = Config {
        space: asked.space,
        pool: asked.pool,
        max_k: asked.max_k,
        tool_desc: asked.tool_desc,
        ..daemon.clone()
    };
    let service = AgmemService::new(db, embedder, Arc::new(session));

    // rmcp gets the *buffered* reader, not the raw half. `read_line` reads
    // ahead, so a client that put `initialize` in the same write as the
    // handshake would otherwise lose it inside a buffer nobody drains again —
    // and a byte pump has no framing to resync on, so it would present as a
    // session that hangs at startup.
    let running = service.serve((read, write)).await?;
    let reason = running.waiting().await?;
    tracing::info!(?reason, "session detached");
    Ok(SessionEnd::Detached)
}

/// Keep the data dir to its owner: the socket has no password, and the
/// boundary around it is the account that can reach it.
fn restrict(data_dir: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;

    if let Err(error) = std::fs::set_permissions(data_dir, std::fs::Permissions::from_mode(0o700)) {
        tracing::warn!(%error, "cannot restrict the data dir to this user");
    }
}
