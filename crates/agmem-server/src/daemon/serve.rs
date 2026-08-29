//! The process that owns the store.
//!
//! It holds the data-dir lock for its whole life, opens the store and the
//! embedder once, and then serves one [`AgmemService`] per attached session
//! over a Unix socket. Sessions come and go; the store does not.

use std::sync::Arc;
use std::time::Duration;

use agmem_embed::Embedder;
use agmem_store::db::Db;
use anyhow::Context;
use rmcp::ServiceExt;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;

use crate::config::Config;
use crate::daemon::{Handshake, socket_path};
use crate::service::AgmemService;
use crate::{embedder, lock};

/// Own the store and serve every session that attaches, until nothing has
/// been attached for `idle_timeout`.
///
/// # Errors
/// When the lock is already held, the store will not open or migrate, the
/// embedder will not load, or the socket cannot be bound.
pub async fn run(cfg: Config) -> anyhow::Result<()> {
    let path = socket_path(&cfg.data_dir)?;
    // The lock is what makes "one owner" true; the socket only advertises it.
    // Holding it here is also what makes the unlink below safe — no other
    // daemon can be alive to have the socket we are about to replace.
    let _lock = lock::acquire(&cfg.data_dir)?;
    restrict(&cfg.data_dir);

    let db = agmem_store::db::connect(&cfg.db_url).await?;
    let schema = agmem_store::migrate::ensure(&db).await?;
    let embedder = embedder::build(&cfg)?;
    agmem_store::migrate::ensure_embedder(&db, embedder.model_id(), embedder.dim()).await?;
    // A daemon is where the sweep belongs: it is the process a machine starts
    // once, and the sessions attaching to it never start anything.
    let pruned = crate::startup::prune(&db).await;

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
        socket = %path.display(),
        "shared store ready"
    );

    let idle = Duration::from_secs(cfg.idle_timeout);
    let daemon = Arc::new(cfg);
    let (ended, mut endings) = mpsc::unbounded_channel::<()>();
    let mut attached: u32 = 0;

    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _) = accepted.context("accepting on the agmem socket")?;
                attached += 1;
                let (db, embedder, daemon, ended) =
                    (db.clone(), Arc::clone(&embedder), Arc::clone(&daemon), ended.clone());
                tokio::spawn(async move {
                    if let Err(error) = session(stream, db, embedder, &daemon).await {
                        tracing::warn!(error = %format!("{error:#}"), "session ended badly");
                    }
                    // The receiver lives as long as the loop, so this only
                    // fails once we are already shutting down.
                    let _ = ended.send(());
                });
            }
            Some(()) = endings.recv() => {
                attached = attached.saturating_sub(1);
                tracing::debug!(attached, "session detached");
            }
            () = idle_elapsed(idle, attached == 0) => {
                tracing::info!(?idle, "nothing attached; shutting down");
                break;
            }
        }
    }

    drop(listener);
    let _ = std::fs::remove_file(&path);
    Ok(())
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

/// One attached session: find out who is asking, then hand rmcp the socket.
async fn session(
    stream: UnixStream,
    db: Db,
    embedder: Arc<dyn Embedder>,
    daemon: &Config,
) -> anyhow::Result<()> {
    let (read, write) = stream.into_split();
    let mut read = BufReader::new(read);
    let mut line = String::new();
    let bytes = read
        .read_line(&mut line)
        .await
        .context("reading the session handshake")?;
    if bytes == 0 {
        // Connect-and-leave is how `--doctor` asks whether a daemon is here.
        tracing::debug!("a probe attached and left without a handshake");
        return Ok(());
    }
    let asked: Handshake = serde_json::from_str(line.trim())
        .context("the session handshake is not the JSON this daemon expects")?;
    Handshake::new(daemon).accept(&asked)?;

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
    Ok(())
}

/// Keep the data dir to its owner: the socket has no password, and the
/// boundary around it is the account that can reach it.
fn restrict(data_dir: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;

    if let Err(error) = std::fs::set_permissions(data_dir, std::fs::Permissions::from_mode(0o700)) {
        tracing::warn!(%error, "cannot restrict the data dir to this user");
    }
}
