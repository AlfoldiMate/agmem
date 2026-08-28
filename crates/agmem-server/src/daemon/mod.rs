//! The shared store daemon (issue #37, design §5.1).
//!
//! Claude Code starts one MCP server per session and every project shares one
//! data directory, but an embedded SurrealKV store is single-writer. So the
//! second concurrent session used to exit on the lock before it served
//! anything, and from inside the client that looks like nothing at all: no
//! error the model can see, the memory tools simply absent.
//!
//! One process owns the store and the rest talk to it. What travels between
//! them is MCP itself — rmcp takes any `AsyncRead + AsyncWrite` as a
//! transport, so a Unix socket *is* an MCP transport and neither side needs a
//! second protocol to learn. A session's `agmem` becomes a byte pump between
//! its stdio and `<data dir>/agmem.sock`; the daemon behind that socket runs
//! the real service.
//!
//! The store is shared but the *configuration* is not: `AGMEM_SPACE` is
//! per-project and `space` is what every tool defaults to. So each connection
//! opens with a [`Handshake`] saying who is asking, and the daemon builds a
//! fresh service for it over the one store.
//!
//! All of this is Unix-only. Elsewhere agmem keeps the single-writer behaviour
//! it has always had.

#[cfg(unix)]
pub mod client;
#[cfg(unix)]
pub mod serve;

use std::path::{Path, PathBuf};

use agmem_core::SpaceName;
use anyhow::bail;
use serde::{Deserialize, Serialize};

use crate::config::{Config, EmbedderKind};

/// The socket the daemon listens on, inside the data dir.
pub const SOCKET_FILE: &str = "agmem.sock";

/// The lock that serialises *starting* a daemon, so a burst of sessions
/// produces one rather than one each.
pub const SPAWN_LOCK_FILE: &str = "agmem.spawn.lock";

/// Where the daemon logs when the session that started it has no log file:
/// a detached process with nowhere to write is one you cannot diagnose.
pub const DAEMON_LOG_FILE: &str = "daemon.log";

/// Bumped whenever the handshake or the socket's meaning changes. A session
/// that meets a daemon it does not recognise refuses rather than guesses.
pub const PROTOCOL_VERSION: u32 = 1;

/// The longest socket path a `sockaddr_un` can hold. macOS allows 104 bytes
/// including the terminator and Linux 108; the smaller number travels.
const MAX_SOCKET_PATH: usize = 104;

/// Whether this run should go through the shared daemon.
///
/// Three runs do not: a remote engine already has a server between the
/// sessions, a `mem://` store is per-process by definition and has nothing to
/// share, and `--no-daemon` is the escape hatch for anyone who wants the old
/// one-process-one-store behaviour back.
pub fn wanted(cfg: &Config) -> bool {
    !cfg.no_daemon && !cfg.db_is_remote() && !cfg.db_url.starts_with("mem://")
}

/// Where the daemon for `data_dir` listens.
///
/// # Errors
/// When the path will not fit a socket address. Truncating it silently would
/// point two data dirs at one socket, which is worse than refusing to start.
pub fn socket_path(data_dir: &Path) -> anyhow::Result<PathBuf> {
    let path = data_dir.join(SOCKET_FILE);
    let len = path.as_os_str().as_encoded_bytes().len();
    if len >= MAX_SOCKET_PATH {
        bail!(
            "the shared-store socket path would be {len} bytes, past the \
             {MAX_SOCKET_PATH} a unix socket address holds: {}. Use a shorter --data, \
             or --no-daemon to open the store in this process.",
            path.display()
        );
    }
    Ok(path)
}

/// The lock a session takes before starting a daemon.
pub fn spawn_lock_path(data_dir: &Path) -> PathBuf {
    data_dir.join(SPAWN_LOCK_FILE)
}

/// What a session tells the daemon before MCP starts.
///
/// `db_url` and `embedder` are not *applied*, they are **checked**: a session
/// asking for a different store or a different model than the running daemon
/// holds is a misconfiguration, and quietly serving the wrong one is worse
/// than refusing. `space`, `pool` and `max_k` are applied — they are what
/// differs legitimately between two projects sharing a store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Handshake {
    /// [`PROTOCOL_VERSION`] of the session that opened the connection.
    pub version: u32,
    /// The store the session expects to be talking to.
    pub db_url: String,
    /// The embedding backend it expects.
    pub embedder: EmbedderKind,
    /// The project asking — what `space` defaults to for this connection.
    pub space: SpaceName,
    /// Candidate pool for this connection's recalls.
    pub pool: u16,
    /// Ceiling on `k` for this connection.
    pub max_k: u16,
}

impl Handshake {
    /// What this configuration asks a daemon for.
    pub fn new(cfg: &Config) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            db_url: cfg.db_url.clone(),
            embedder: cfg.embedder,
            space: cfg.space.clone(),
            pool: cfg.pool,
            max_k: cfg.max_k,
        }
    }

    /// Refuse a session that expects something this daemon is not serving.
    ///
    /// # Errors
    /// When the protocol version, the store or the embedder disagree. The
    /// message is the session's to read, so it names both sides.
    pub fn accept(&self, asked: &Self) -> anyhow::Result<()> {
        if asked.version != self.version {
            bail!(
                "this session speaks agmem daemon protocol v{} and the running daemon \
                 speaks v{}; the daemon is an older or newer agmem. Stop it and let this \
                 one start a fresh daemon.",
                asked.version,
                self.version
            );
        }
        if asked.db_url != self.db_url {
            bail!(
                "the running daemon holds {} and this session asked for {}. One data dir \
                 is one store; pass --no-daemon, or point both at the same --db.",
                self.db_url,
                asked.db_url
            );
        }
        if asked.embedder != self.embedder {
            bail!(
                "the running daemon embeds with {} and this session asked for {}. Vectors \
                 from two models cannot be compared; stop the daemon or match --embedder.",
                self.embedder.as_str(),
                asked.embedder.as_str()
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(data_dir: &str) -> Config {
        use clap::Parser;
        crate::config::Cli::try_parse_from(["agmem", "--data", data_dir])
            .expect("parse")
            .resolve()
            .expect("resolve")
    }

    #[test]
    fn a_handshake_survives_the_wire() {
        let cfg = config("/tmp/agmem-test");
        let sent = Handshake::new(&cfg);
        let line = serde_json::to_string(&sent).expect("serialize");
        assert!(!line.contains('\n'), "the handshake is one line: {line}");
        assert_eq!(
            serde_json::from_str::<Handshake>(&line).expect("deserialize"),
            sent
        );
    }

    #[test]
    fn a_daemon_refuses_a_session_that_expects_another_store() {
        let daemon = Handshake::new(&config("/tmp/agmem-test"));

        let mut asked = daemon.clone();
        asked.space = SpaceName::new("other-project").expect("space");
        asked.pool = 8;
        daemon
            .accept(&asked)
            .expect("a different project is the whole point of sharing");

        for (label, broken) in [
            (
                "version",
                Handshake {
                    version: daemon.version + 1,
                    ..daemon.clone()
                },
            ),
            (
                "store",
                Handshake {
                    db_url: "surrealkv:///elsewhere".to_owned(),
                    ..daemon.clone()
                },
            ),
            (
                "embedder",
                Handshake {
                    embedder: EmbedderKind::None,
                    ..daemon.clone()
                },
            ),
        ] {
            let error = daemon
                .accept(&broken)
                .expect_err("{label} disagreeing must be refused");
            assert!(
                !error.to_string().is_empty(),
                "{label} must say what to do about it"
            );
        }
    }

    #[test]
    fn a_socket_path_that_would_not_fit_is_refused_rather_than_truncated() {
        let long = PathBuf::from("/tmp").join("x".repeat(MAX_SOCKET_PATH));
        let error = socket_path(&long).expect_err("too long");
        assert!(
            error.to_string().contains("--no-daemon"),
            "the refusal names the way out: {error}"
        );
        socket_path(Path::new("/tmp/agmem")).expect("an ordinary path fits");
    }
}
