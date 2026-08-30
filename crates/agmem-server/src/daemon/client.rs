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
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;

use crate::config::Config;
use crate::daemon::{DAEMON_LOG_FILE, Handshake, socket_path, spawn_lock_path};

/// How long to queue for the right to start a daemon before giving up. Long
/// enough for another session to finish starting one, short enough that a
/// wedged lock does not look like a hang.
const SPAWN_LOCK_DEADLINE: Duration = Duration::from_secs(30);

/// How long a starting daemon has to reach the point of accepting. The first
/// run of a fresh install downloads the ONNX model, which dominates this.
const READY_DEADLINE: Duration = Duration::from_secs(120);

/// How often to re-try the socket while waiting.
const POLL: Duration = Duration::from_millis(50);

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
/// configuration's [`Handshake`] already sent. What flows next is MCP;
/// [`run`] pumps stdio into it, `agmem context` (issue #46) speaks it itself.
///
/// # Errors
/// When no daemon can be reached or started, or the handshake will not land.
pub async fn attach(cfg: &Config) -> anyhow::Result<UnixStream> {
    let path = socket_path(&cfg.data_dir)?;
    let mut stream = match connect(&path).await {
        Some(stream) => stream,
        None => start_one(cfg, &path).await?,
    };
    let mut handshake = serde_json::to_vec(&Handshake::new(cfg))?;
    handshake.push(b'\n');
    stream
        .write_all(&handshake)
        .await
        .context("the shared store closed before the handshake landed")?;
    Ok(stream)
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
    let log_file = cfg
        .log_file
        .clone()
        .unwrap_or_else(|| cfg.data_dir.join(DAEMON_LOG_FILE));

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
    let log = cfg
        .log_file
        .clone()
        .unwrap_or_else(|| cfg.data_dir.join(DAEMON_LOG_FILE));
    bail!(
        "the shared store did not come up within {READY_DEADLINE:?}. Its log is {}; \
         --no-daemon opens the store in this process instead.",
        log.display()
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
