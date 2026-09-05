//! The session side: find the daemon, start one if there is none, then get
//! out of the way.
//!
//! Once connected this process does nothing but move bytes. Every decision —
//! what a tool does, which spaces it reads — happens in the daemon; the only
//! thing this side contributes is the [`Handshake`] saying which project is
//! asking.

use std::fs::{File, TryLockError};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use crate::config::Config;
use crate::daemon::{Ack, DAEMON_LOG_FILE, Handshake, RELEASE, socket_path, spawn_lock_path};
use crate::lock;

/// How long to queue for the right to start a daemon before giving up. Long
/// enough for another session to finish starting one, short enough that a
/// wedged lock does not look like a hang.
const SPAWN_LOCK_DEADLINE: Duration = Duration::from_secs(30);

/// How long a starting daemon has to reach the point of accepting. The first
/// run of a fresh install downloads the ONNX model, which dominates this.
const READY_DEADLINE: Duration = Duration::from_secs(120);

/// How often to re-try the socket while waiting.
const POLL: Duration = Duration::from_millis(50);

/// How long a retiring daemon gets to unlink its socket and release the
/// store lock before this session starts a fresh one anyway.
const RETIRE_DEADLINE: Duration = Duration::from_secs(15);

/// How long the daemon has to answer the handshake (issue #112). A daemon
/// that accepted the connection and says nothing is wedged, not slow: the
/// ack is one line written before any work, so a read that outlives this is
/// a session that would otherwise hang with no memory tools and no message.
const ACK_DEADLINE: Duration = Duration::from_secs(10);

/// Attach this session to the shared store, starting the daemon if needed,
/// and pump stdio until the client goes away.
///
/// # Errors
/// When no daemon can be reached or started. The caller exits non-zero on
/// that rather than opening the store here — a second writer on an embedded
/// store is exactly what this exists to prevent.
pub async fn run(cfg: &Config) -> anyhow::Result<()> {
    let stream = attach(cfg).await?;
    pump(stream).await
}

/// A connection to the shared daemon — found, or started — with this
/// configuration's [`Handshake`] sent and the daemon's [`Ack`] consumed, so
/// every byte after it is MCP. [`run`] pumps stdio into it, `agmem context`
/// (issue #46) speaks it itself.
///
/// A daemon from an older release answers "retiring" and shuts down (issue
/// #60); this session waits it out and starts a fresh daemon from its own
/// binary — once. Two retirements in a row means two binaries are fighting
/// over the socket, and that is the user's to resolve.
///
/// # Errors
/// When no daemon can be reached or started, or the daemon refuses the
/// handshake. The refusal's message is surfaced here — never swallowed into
/// a clean exit — because the alternative is a session with no memory tools
/// and no explanation.
pub async fn attach(cfg: &Config) -> anyhow::Result<UnixStream> {
    let path = socket_path(&cfg.data_dir)?;
    match attach_once(cfg, &path, Takeover::No).await? {
        Attached::Serving(stream) => Ok(stream),
        Attached::DaemonRetired => {
            wait_for_retirement(&path).await;
            match attach_once(cfg, &path, Takeover::FromRetired).await? {
                Attached::Serving(stream) => Ok(stream),
                Attached::DaemonRetired => bail!(
                    "the shared store on {} retired twice in a row: two different agmem \
                     builds are attaching to it. Align them on one release, or pass \
                     --no-daemon.",
                    path.display()
                ),
            }
        }
    }
}

/// What one handshake attempt came to.
enum Attached {
    /// The daemon took the session; the stream is MCP from here.
    Serving(UnixStream),
    /// The daemon is from another release and is shutting down so this
    /// session can start one from its own binary.
    DaemonRetired,
}

/// Whether a daemon this session starts is replacing one that just retired.
///
/// It changes one thing: the fresh daemon's ready line, which then says that
/// the sessions still attached to the old daemon need a restart (issue
/// #112). That fact is otherwise recorded nowhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Takeover {
    /// The ordinary start: nothing was serving.
    No,
    /// The daemon that was serving retired for this session's release.
    FromRetired,
}

/// Connect or start a daemon, hand it the handshake, and read its verdict.
async fn attach_once(cfg: &Config, path: &Path, takeover: Takeover) -> anyhow::Result<Attached> {
    let mut stream = match connect(path).await {
        Some(stream) => stream,
        None => start_one(cfg, path, takeover).await?,
    };
    let mut handshake = serde_json::to_vec(&Handshake::new(cfg))?;
    handshake.push(b'\n');
    stream
        .write_all(&handshake)
        .await
        .context("the shared store closed before the handshake landed")?;

    let ack = read_ack(&mut stream, cfg, ACK_DEADLINE).await?;
    if ack.ok {
        return Ok(Attached::Serving(stream));
    }
    let why = ack
        .error
        .unwrap_or_else(|| "the shared store refused the handshake without saying why".to_owned());
    if ack.retiring {
        tracing::info!(
            %why,
            "waiting for the old daemon to retire; sessions still attached to it need a restart"
        );
        return Ok(Attached::DaemonRetired);
    }
    bail!(why)
}

/// The daemon's one-line answer to the handshake, read a byte at a time: the
/// bytes after the newline are MCP and belong to the pump, so nothing here
/// may buffer past it.
///
/// Bounded by `deadline` (issue #112): before it, a daemon that accepted and
/// then hung — mid-migration on a store it could not finish opening, say —
/// held every new session here forever.
async fn read_ack(
    stream: &mut UnixStream,
    cfg: &Config,
    deadline: Duration,
) -> anyhow::Result<Ack> {
    match tokio::time::timeout(deadline, read_ack_line(stream, cfg)).await {
        Ok(ack) => ack,
        Err(_elapsed) => bail!(
            "the shared store accepted the connection but did not answer the handshake \
             within {deadline:?}. It is wedged rather than slow{}; stop it and retry. Its \
             log is {}.",
            holder_hint(cfg),
            log_path(cfg).display()
        ),
    }
}

async fn read_ack_line(stream: &mut UnixStream, cfg: &Config) -> anyhow::Result<Ack> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let n = stream
            .read(&mut byte)
            .await
            .context("reading the shared store's handshake ack")?;
        if n == 0 {
            // Pre-#60 daemons never wrote an ack: a refusal was a close the
            // client pumped through and exited 0 on. Meeting that close here
            // turns it into the message it always should have been — and
            // since a daemon that old cannot be retired over the wire, the
            // message carries the pid to stop by hand (issue #112).
            bail!(
                "the shared store closed without answering the handshake. It is probably \
                 an agmem older than {RELEASE} still holding {}{}; stop that process (or \
                 wait out its idle timeout) and retry. Its log is {}.",
                cfg.data_dir.display(),
                holder_hint(cfg),
                log_path(cfg).display()
            );
        }
        if byte[0] == b'\n' {
            break;
        }
        line.push(byte[0]);
        if line.len() > 4096 {
            bail!("the shared store's handshake ack never ended");
        }
    }
    serde_json::from_slice(&line).context("the shared store's handshake ack is not JSON")
}

/// The pid on the store lock, worded to drop into a message — with the
/// command to run, because a session that has to kill a daemon by hand
/// should not also have to work out how.
fn holder_hint(cfg: &Config) -> String {
    match lock::owner(&cfg.data_dir) {
        Some(pid) => format!(" (pid {pid} by its lock file: `kill {pid}`)"),
        None => String::new(),
    }
}

/// Wait for a retiring daemon to unlink its socket. Its store lock goes
/// with its process, a little later; [`start_one`] waits for that.
async fn wait_for_retirement(path: &Path) {
    let deadline = Instant::now() + RETIRE_DEADLINE;
    while Instant::now() < deadline && path.exists() {
        tokio::time::sleep(POLL).await;
    }
}

/// Wait for the store lock to be free, so the daemon about to be started
/// does not die on it.
///
/// A retiring daemon drains its sessions for a moment after it unlinks the
/// socket; the lock is what says its process is actually gone (issue #112 —
/// before this, a fixed sleep guessed, and a guess too short cost the
/// session the whole `READY_DEADLINE` before it learned anything). A lock
/// still held past the deadline is a process that is not going anywhere: a
/// `--no-daemon` session, or a daemon that stopped answering.
///
/// A daemon from before issue #124 released this lock a few milliseconds
/// before it let go of the store itself; [`start_one`] allows one more start
/// for that gap.
///
/// # Errors
/// When the lock is still held after `RETIRE_DEADLINE`.
async fn wait_for_store_lock(cfg: &Config) -> anyhow::Result<()> {
    let deadline = Instant::now() + RETIRE_DEADLINE;
    loop {
        if lock::probe(&cfg.data_dir)? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!(
                "another agmem process{} still owns the store in {} after {RETIRE_DEADLINE:?}, \
                 and no daemon is answering on its socket. It is a --no-daemon session or a \
                 daemon that stopped serving; stop it, or pass --no-daemon here.",
                holder_hint(cfg),
                cfg.data_dir.display()
            );
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Where the daemon this configuration would start writes its log.
fn log_path(cfg: &Config) -> std::path::PathBuf {
    cfg.log_file
        .clone()
        .unwrap_or_else(|| cfg.data_dir.join(DAEMON_LOG_FILE))
}

/// A daemon that answers, or nothing. Every failure is "not there" — the
/// caller's next move is the same whether the socket is missing, stale, or
/// refusing.
async fn connect(path: &Path) -> Option<UnixStream> {
    UnixStream::connect(path).await.ok()
}

/// Take the spawn lock, start a daemon, and wait for it to accept.
async fn start_one(cfg: &Config, path: &Path, takeover: Takeover) -> anyhow::Result<UnixStream> {
    std::fs::create_dir_all(&cfg.data_dir)
        .with_context(|| format!("cannot create data dir {}", cfg.data_dir.display()))?;
    let _guard = queue_to_spawn(&spawn_lock_path(&cfg.data_dir)).await?;

    // Somebody else may have started one while we queued. This is the whole
    // reason for the lock: a burst of sessions has to produce one daemon.
    if let Some(stream) = connect(path).await {
        return Ok(stream);
    }
    // A socket file nothing answers is a daemon that died. Left in place it
    // fails every future bind with EADDRINUSE, so the dead one goes now.
    if path.exists() {
        std::fs::remove_file(path)
            .with_context(|| format!("cannot clear the stale socket {}", path.display()))?;
    }
    // Nothing answers on the socket, but the store may still be held — by
    // the daemon that just retired and is draining, most often. A daemon
    // started now would exit on the lock, and this session would learn that
    // only from the ready timeout.
    wait_for_store_lock(cfg).await?;

    // A daemon that retired from a release before issue #124 released the
    // data-dir lock a moment before the store's own lock, and a daemon
    // started in that gap dies on the store. One more start, once the gap
    // has closed, is the whole allowance: a second death is a real error.
    let mut starts_left = match takeover {
        Takeover::No => 1,
        Takeover::FromRetired => 2,
    };
    loop {
        let mut child = spawn(cfg, takeover)?;
        match wait_until_ready(cfg, path, &mut child).await? {
            Ready::Serving(stream) => {
                reap_in_background(child);
                return Ok(stream);
            }
            Ready::Died(status) => {
                starts_left -= 1;
                if starts_left == 0 {
                    bail!(
                        "the shared store exited ({status}) before it accepted a session. \
                         Its log is {}; --no-daemon opens the store in this process instead.",
                        log_path(cfg).display()
                    );
                }
                tracing::info!(
                    %status,
                    "the shared store died at startup right after a retirement; starting it once more"
                );
                wait_for_store_lock(cfg).await?;
            }
        }
    }
}

/// What became of a daemon this session started.
enum Ready {
    /// It accepted; the stream is the session's.
    Serving(UnixStream),
    /// It exited before it accepted. Its reason is in its log, not here: its
    /// stderr was closed at spawn.
    Died(ExitStatus),
}

/// Collect the daemon's exit status when it comes, so the process does not
/// linger as a zombie for as long as this session lives (issue #124). The
/// daemon is in its own process group and outlives this session as a rule;
/// then the thread simply ends with the session.
fn reap_in_background(mut child: Child) {
    std::thread::spawn(move || {
        let _ = child.wait();
    });
}

/// Wait for the right to start a daemon.
///
/// The lock is held only across the start, not for the session, so queuing
/// behind it is short. A holder that crashed releases it with its process.
async fn queue_to_spawn(path: &Path) -> anyhow::Result<File> {
    let file = File::options()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
        .with_context(|| format!("cannot open {}", path.display()))?;

    let deadline = Instant::now() + SPAWN_LOCK_DEADLINE;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                tokio::time::sleep(POLL).await;
            }
            Err(TryLockError::WouldBlock) => bail!(
                "waited {SPAWN_LOCK_DEADLINE:?} for another agmem to finish starting the \
                 shared store and it never did. Remove {} if no agmem is running, or pass \
                 --no-daemon.",
                path.display()
            ),
            Err(TryLockError::Error(error)) => {
                return Err(error).with_context(|| format!("cannot lock {}", path.display()));
            }
        }
    }
}

/// Start a detached daemon from this same binary.
fn spawn(cfg: &Config, takeover: Takeover) -> anyhow::Result<Child> {
    let exe = std::env::current_exe()
        .context("cannot find this binary to start the shared store from")?;
    let log_file = log_path(cfg);

    let mut command = Command::new(exe);
    command
        .arg("--daemon-serve")
        .arg("--data")
        .arg(&cfg.data_dir)
        .arg("--db")
        .arg(&cfg.db_url)
        .arg("--embedder")
        .arg(cfg.embedder.as_str())
        .arg("--accelerator")
        .arg(cfg.accelerator.as_str())
        .arg("--idle-timeout")
        .arg(cfg.idle_timeout.to_string())
        .arg("--log")
        .arg(&cfg.log)
        // A detached process with nowhere to write is one nobody can diagnose,
        // so its log goes to a file rather than to the closed streams below.
        .arg("--log-file")
        .arg(&log_file)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if takeover == Takeover::FromRetired {
        command.arg("--took-over");
    }

    // Its own process group. The daemon has to outlive the session that
    // happened to start it, including a Ctrl-C in the terminal that owns it.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }

    let child = command.spawn().with_context(|| {
        format!(
            "cannot start the shared store; its log is {}",
            log_file.display()
        )
    })?;
    tracing::info!(log = %log_file.display(), ?takeover, "started the shared store");
    Ok(child)
}

/// Poll the socket until the daemon we started accepts, exits, or time runs
/// out.
///
/// The exit is checked on every turn (issue #124): a daemon that died at
/// startup — on the store's lock, most often — used to cost the session the
/// whole `READY_DEADLINE`, with nothing to read at the end of it.
///
/// # Errors
/// When the deadline passes with the daemon alive and silent, or the child
/// cannot be checked on.
async fn wait_until_ready(cfg: &Config, path: &Path, child: &mut Child) -> anyhow::Result<Ready> {
    let deadline = Instant::now() + READY_DEADLINE;
    while Instant::now() < deadline {
        if let Some(stream) = connect(path).await {
            return Ok(Ready::Serving(stream));
        }
        if let Some(status) = child
            .try_wait()
            .context("checking whether the shared store is still starting")?
        {
            return Ok(Ready::Died(status));
        }
        tokio::time::sleep(POLL).await;
    }
    bail!(
        "the shared store did not come up within {READY_DEADLINE:?}. Its log is {}; \
         --no-daemon opens the store in this process instead.",
        log_path(cfg).display()
    )
}

/// Move bytes between this process's stdio and the daemon, until either end
/// stops.
async fn pump(stream: UnixStream) -> anyhow::Result<()> {
    let (mut from_daemon, mut to_daemon) = stream.into_split();
    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();

    // Whichever direction ends first ends the session: the client closing
    // stdin is how a session finishes, and the daemon closing the socket is
    // one this process cannot continue without.
    tokio::select! {
        result = tokio::io::copy(&mut stdin, &mut to_daemon) => {
            result.context("relaying to the shared store")?;
            tracing::info!("the client hung up; detaching from the shared store");
        }
        result = tokio::io::copy(&mut from_daemon, &mut stdout) => {
            result.context("relaying from the shared store")?;
            tracing::info!("the shared store closed the session");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(data: &Path) -> Config {
        use clap::Parser as _;
        crate::config::Cli::try_parse_from([
            "agmem",
            "--embedder",
            "none",
            "--data",
            &data.display().to_string(),
        ])
        .expect("parse")
        .resolve()
        .expect("resolve")
    }

    #[tokio::test]
    async fn a_daemon_that_accepts_and_says_nothing_is_reported_not_waited_on() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = config(dir.path());
        let path = socket_path(dir.path()).expect("socket path");
        let listener = tokio::net::UnixListener::bind(&path).expect("bind");
        // Accept and hold: what a daemon wedged between accept and ack does.
        let wedged = tokio::spawn(async move {
            let (held, _) = listener.accept().await.expect("accept");
            std::future::pending::<()>().await;
            drop(held);
        });

        let mut stream = UnixStream::connect(&path).await.expect("connect");
        let error = read_ack(&mut stream, &cfg, Duration::from_millis(100))
            .await
            .expect_err("no ack within the deadline is an error");
        let message = format!("{error:#}");
        assert!(
            message.contains("did not answer the handshake"),
            "the message says what happened: {message}"
        );
        assert!(
            message.contains(&cfg.data_dir.join(DAEMON_LOG_FILE).display().to_string()),
            "and where to look: {message}"
        );
        wedged.abort();
    }

    #[tokio::test]
    async fn a_close_without_an_ack_names_the_pid_on_the_lock() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = config(dir.path());
        let path = socket_path(dir.path()).expect("socket path");
        let listener = tokio::net::UnixListener::bind(&path).expect("bind");
        // What a pre-v3 daemon does with a handshake it refuses: close.
        tokio::spawn(async move {
            let (accepted, _) = listener.accept().await.expect("accept");
            drop(accepted);
        });
        std::fs::write(dir.path().join(lock::LOCK_FILE), "4242\n").expect("a lock file");

        let mut stream = UnixStream::connect(&path).await.expect("connect");
        let error = read_ack(&mut stream, &cfg, ACK_DEADLINE)
            .await
            .expect_err("a close is not an ack");
        let message = format!("{error:#}");
        assert!(
            message.contains("kill 4242"),
            "the way out is a command, not a hunt: {message}"
        );
    }

    #[tokio::test]
    async fn a_daemon_that_exits_at_startup_is_reported_at_once_with_its_log() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = config(dir.path());
        let path = socket_path(dir.path()).expect("socket path");

        // `spawn` starts this same binary — here the test executable, which
        // does not know `--daemon-serve` and exits at once: exactly a daemon
        // that died at startup (issue #124).
        let started = Instant::now();
        let error = start_one(&cfg, &path, Takeover::No)
            .await
            .expect_err("a child that exited never accepts");
        let message = format!("{error:#}");
        assert!(
            started.elapsed() < READY_DEADLINE / 4,
            "the exit is seen when it happens, not at the ready deadline: {:?}",
            started.elapsed()
        );
        assert!(
            message.contains("exited"),
            "the message says what happened: {message}"
        );
        assert!(
            message.contains(&log_path(&cfg).display().to_string()),
            "and where the daemon wrote why: {message}"
        );
    }
}
