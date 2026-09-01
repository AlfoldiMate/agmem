//! The session side: find the daemon, start one if there is none, then get
//! out of the way.
//!
//! Once connected this process does nothing but move bytes. Every decision —
//! what a tool does, which spaces it reads — happens in the daemon; the only
//! thing this side contributes is the [`Handshake`] saying which project is
//! asking.

use std::fs::{File, TryLockError};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use crate::config::Config;
use crate::daemon::{Ack, DAEMON_LOG_FILE, Handshake, RELEASE, socket_path, spawn_lock_path};

/// How long to queue for the right to start a daemon before giving up. Long
/// enough for another session to finish starting one, short enough that a
/// wedged lock does not look like a hang.
const SPAWN_LOCK_DEADLINE: Duration = Duration::from_secs(30);

/// How long a starting daemon has to reach the point of accepting. The first
/// run of a fresh install downloads the ONNX model, which dominates this.
const READY_DEADLINE: Duration = Duration::from_secs(120);

/// How often to re-try the socket while waiting.
const POLL: Duration = Duration::from_millis(50);

/// How long a retiring daemon gets to unlink its socket before this session
/// starts a fresh one anyway.
const RETIRE_DEADLINE: Duration = Duration::from_secs(15);

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
/// A daemon from another release answers "retiring" and shuts down (issue
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
    match attach_once(cfg, &path).await? {
        Attached::Serving(stream) => Ok(stream),
        Attached::DaemonRetired => {
            wait_for_retirement(&path).await;
            match attach_once(cfg, &path).await? {
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

/// Connect or start a daemon, hand it the handshake, and read its verdict.
async fn attach_once(cfg: &Config, path: &Path) -> anyhow::Result<Attached> {
    let mut stream = match connect(path).await {
        Some(stream) => stream,
        None => start_one(cfg, path).await?,
    };
    let mut handshake = serde_json::to_vec(&Handshake::new(cfg))?;
    handshake.push(b'\n');
    stream
        .write_all(&handshake)
        .await
        .context("the shared store closed before the handshake landed")?;

    let ack = read_ack(&mut stream, cfg).await?;
    if ack.ok {
        return Ok(Attached::Serving(stream));
    }
    let why = ack
        .error
        .unwrap_or_else(|| "the shared store refused the handshake without saying why".to_owned());
    if ack.retiring {
        tracing::info!(%why, "waiting for the old daemon to retire");
        return Ok(Attached::DaemonRetired);
    }
    bail!(why)
}

/// The daemon's one-line answer to the handshake, read a byte at a time: the
/// bytes after the newline are MCP and belong to the pump, so nothing here
/// may buffer past it.
async fn read_ack(stream: &mut UnixStream, cfg: &Config) -> anyhow::Result<Ack> {
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
            // turns it into the message it always should have been.
            bail!(
                "the shared store closed without answering the handshake. It is probably \
                 an agmem older than {RELEASE} still holding {}; stop that process (or \
                 wait out its idle timeout) and retry. Its log is {}.",
                cfg.data_dir.display(),
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

/// Wait for a retiring daemon to unlink its socket, then a beat longer for
/// its store lock to release with its process — the fresh daemon this
/// session is about to start needs both gone.
async fn wait_for_retirement(path: &Path) {
    let deadline = Instant::now() + RETIRE_DEADLINE;
    while Instant::now() < deadline && path.exists() {
        tokio::time::sleep(POLL).await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
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
async fn start_one(cfg: &Config, path: &Path) -> anyhow::Result<UnixStream> {
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

    spawn(cfg)?;
    wait_until_ready(cfg, path).await
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
fn spawn(cfg: &Config) -> anyhow::Result<()> {
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

    // Its own process group. The daemon has to outlive the session that
    // happened to start it, including a Ctrl-C in the terminal that owns it.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }

    command.spawn().with_context(|| {
        format!(
            "cannot start the shared store; its log is {}",
            log_file.display()
        )
    })?;
    tracing::info!(log = %log_file.display(), "started the shared store");
    Ok(())
}

/// Poll the socket until the daemon we started accepts, or time runs out.
async fn wait_until_ready(cfg: &Config, path: &Path) -> anyhow::Result<UnixStream> {
    let deadline = Instant::now() + READY_DEADLINE;
    while Instant::now() < deadline {
        if let Some(stream) = connect(path).await {
            return Ok(stream);
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
