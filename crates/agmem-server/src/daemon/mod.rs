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

use std::cmp::Ordering;
use std::path::{Path, PathBuf};

use agmem_core::SpaceName;
use anyhow::bail;
use serde::{Deserialize, Serialize};

use crate::config::{Config, EmbedderKind, ToolDescriptions};

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
///
/// v2 added `tool_desc`. A v1 daemon deserialises the extra field happily and
/// then serves its own descriptions, so the bump is the only thing that turns
/// "your override was ignored" into a message someone can read.
///
/// v3 added `release` and the [`Ack`] the daemon now writes back (issue #60).
/// A v2 daemon reads a v3 handshake, refuses it into its own log, and closes —
/// which the v3 client, waiting on an ack that never comes, reports loudly
/// instead of pumping EOF and exiting clean.
pub const PROTOCOL_VERSION: u32 = 3;

/// The version of the code actually serving. `PROTOCOL_VERSION` only moves
/// when the wire changes, so it cannot tell a v0.1.3 daemon from a v0.1.4
/// one — and a daemon that outlives a release keeps serving the old schema,
/// scoring and tool wording until its idle timeout happens to recycle it
/// (issue #60, and a real incident: a deleted binary's daemon kept serving).
pub const RELEASE: &str = env!("CARGO_PKG_VERSION");

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
/// than refusing. `space`, `pool`, `max_k` and `tool_desc` are applied — they
/// are what differs legitimately between two projects sharing a store.
///
/// `tool_desc` has to travel for the override to work at all: the daemon is
/// started by whichever session got there first, so without this every later
/// project would silently inherit that one's wording along with its store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Handshake {
    /// [`PROTOCOL_VERSION`] of the session that opened the connection.
    pub version: u32,
    /// [`RELEASE`] of the binary that opened the connection. A mismatch means
    /// the daemon is code from another release: it retires so the session can
    /// start one from its own binary, rather than serving stale behaviour.
    pub release: String,
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
    /// The tool descriptions this project wants served, if it replaced any.
    #[serde(default)]
    pub tool_desc: ToolDescriptions,
}

impl Handshake {
    /// What this configuration asks a daemon for.
    pub fn new(cfg: &Config) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            release: RELEASE.to_owned(),
            db_url: cfg.db_url.clone(),
            embedder: cfg.embedder,
            space: cfg.space.clone(),
            pool: cfg.pool,
            max_k: cfg.max_k,
            tool_desc: cfg.tool_desc.clone(),
        }
    }

    /// Refuse a session that expects something this daemon is not serving.
    ///
    /// # Errors
    /// When the release, the protocol version, the store or the embedder
    /// disagree. The message is the session's to read, so it names both
    /// sides; `retire` says whether this daemon should hand the socket over.
    pub fn accept(&self, asked: &Self) -> Result<(), Refusal> {
        // Release first: a version skew subsumes whatever else differs. The
        // *newer* binary is the one the user just installed, so the daemon
        // defers to a newer attacher rather than serving code that no longer
        // matches what is on disk — and keeps serving when the attacher is
        // the older one (issue #112): two installs on PATH would otherwise
        // retire each other's daemon in turn, cutting live sessions each time.
        match newer(&asked.release, &self.release) {
            Some(Ordering::Greater) => {
                return Err(Refusal {
                    retire: true,
                    message: format!(
                        "the running daemon is agmem {} and this session is agmem {}; the \
                         daemon is retiring so a fresh one can serve this release. Sessions \
                         still attached to it need a restart.",
                        self.release, asked.release
                    ),
                });
            }
            Some(Ordering::Less) => {
                return Err(Refusal {
                    retire: false,
                    message: format!(
                        "the running daemon is agmem {} and this session is an older agmem \
                         {}; the daemon keeps serving the newer release. Run this session \
                         from the same agmem, or pass --no-daemon.",
                        self.release, asked.release
                    ),
                });
            }
            Some(Ordering::Equal) => {}
            None => {
                // One side is not a version this code can read. Nothing says
                // which is newer, so the rule from before #112 stands: the
                // attacher's binary is the one on disk.
                return Err(Refusal {
                    retire: true,
                    message: format!(
                        "the running daemon is agmem {} and this session is agmem {}; the \
                         daemon is retiring so a fresh one can serve this release.",
                        self.release, asked.release
                    ),
                });
            }
        }
        if asked.version != self.version {
            return Err(Refusal {
                // Same release but different protocol should be impossible;
                // if it happens anyway, the newer wire wins the socket.
                retire: asked.version > self.version,
                message: format!(
                    "this session speaks agmem daemon protocol v{} and the running daemon \
                     speaks v{}; the daemon is an older or newer agmem. Stop it and let \
                     this one start a fresh daemon.",
                    asked.version, self.version
                ),
            });
        }
        if asked.db_url != self.db_url {
            return Err(Refusal {
                retire: false,
                message: format!(
                    "the running daemon holds {} and this session asked for {}. One data \
                     dir is one store; pass --no-daemon, or point both at the same --db.",
                    self.db_url, asked.db_url
                ),
            });
        }
        if asked.embedder != self.embedder {
            return Err(Refusal {
                retire: false,
                message: format!(
                    "the running daemon embeds with {} and this session asked for {}. \
                     Vectors from two models cannot be compared; stop the daemon or match \
                     --embedder.",
                    self.embedder.as_str(),
                    asked.embedder.as_str()
                ),
            });
        }
        Ok(())
    }
}

/// Why a daemon turned a session away, and whether it is stepping aside.
///
/// `retire` is the difference between "you are misconfigured" (wrong store,
/// wrong embedder — the daemon stays, the session gets an error) and "I am
/// stale" (another release — the daemon shuts down so the refused session can
/// start one from its own binary).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// What to tell the session. Names both sides and the way out.
    pub message: String,
    /// Whether the daemon shuts down after refusing.
    pub retire: bool,
}

impl Refusal {
    /// What a session hears when it attaches to a daemon another session
    /// already retired (issue #112): the same "wait for the fresh one" the
    /// first one got, so it waits instead of coming up on a socket about to
    /// close.
    pub fn already_retiring() -> Self {
        Self {
            retire: true,
            message: format!(
                "the running daemon (agmem {RELEASE}) is retiring for a newer release; \
                 this session waits for the fresh one."
            ),
        }
    }
}

/// Which of two releases is newer, if both are versions this code can read.
///
/// `Greater` means `asked` is newer than `daemon`. Both are `CARGO_PKG_VERSION`
/// strings in practice, so the `None` branch is for a hand-built binary
/// somebody gave a release name that is not one.
fn newer(asked: &str, daemon: &str) -> Option<Ordering> {
    let asked = semver::Version::parse(asked).ok()?;
    let daemon = semver::Version::parse(daemon).ok()?;
    Some(asked.cmp(&daemon))
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Refusal {}

/// The one line the daemon writes back after reading a [`Handshake`], before
/// any MCP flows (issue #60). Without it a refusal was an EOF the client
/// pumped through and exited 0 on — the session came up with no memory tools
/// and no explanation beyond a line in `daemon.log`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ack {
    /// Whether the daemon accepted the session. When `true` the rest of the
    /// stream is MCP; when `false`, `error` says why and the stream ends.
    pub ok: bool,
    /// The refusal, worded for the session that has to act on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The daemon is shutting down so the session can start a fresh one.
    #[serde(default)]
    pub retiring: bool,
}

impl Ack {
    /// The daemon took the session.
    pub fn accepted() -> Self {
        Self {
            ok: true,
            error: None,
            retiring: false,
        }
    }

    /// The daemon turned the session away.
    pub fn refused(refusal: &Refusal) -> Self {
        Self {
            ok: false,
            error: Some(refusal.message.clone()),
            retiring: refusal.retire,
        }
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
        let mut cfg = config("/tmp/agmem-test");
        // A description is several paragraphs, and the handshake is one line.
        // JSON escapes the breaks, so the two are compatible — but only a test
        // keeps them that way.
        cfg.tool_desc = ToolDescriptions::from_iter([("context", "First.\n\nThen the rest.")]);
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
        asked.tool_desc = ToolDescriptions::from_iter([("recall", "Ask the store first.")]);
        daemon
            .accept(&asked)
            .expect("a different project is the whole point of sharing");

        for (label, broken, retires) in [
            (
                "newer release",
                Handshake {
                    release: "999.0.0".to_owned(),
                    ..daemon.clone()
                },
                // A newer release means this daemon is the stale one: it
                // steps aside rather than serving code that no longer
                // matches the binary on disk.
                true,
            ),
            (
                "older release",
                Handshake {
                    release: "0.0.1".to_owned(),
                    ..daemon.clone()
                },
                // An older attacher is a second install on PATH, not an
                // upgrade (issue #112): retiring for it would let the two
                // binaries retire each other's daemon in turn.
                false,
            ),
            (
                "unreadable release",
                Handshake {
                    // Not a version at all — "0.0.0-elsewhere" would be, and
                    // would sort as older.
                    release: "elsewhere".to_owned(),
                    ..daemon.clone()
                },
                // Nothing says which side is newer; the rule from before
                // #112 stands and the attacher's binary wins.
                true,
            ),
            (
                "version",
                Handshake {
                    version: daemon.version + 1,
                    ..daemon.clone()
                },
                true,
            ),
            (
                "store",
                Handshake {
                    db_url: "surrealkv:///elsewhere".to_owned(),
                    ..daemon.clone()
                },
                // A wrong store is the session's misconfiguration; the
                // daemon keeps serving everyone else.
                false,
            ),
            (
                "embedder",
                Handshake {
                    embedder: EmbedderKind::None,
                    ..daemon.clone()
                },
                false,
            ),
        ] {
            let refusal = daemon
                .accept(&broken)
                .expect_err("{label} disagreeing must be refused");
            assert!(
                !refusal.message.is_empty(),
                "{label} must say what to do about it"
            );
            assert_eq!(refusal.retire, retires, "{label}: wrong side gives way");
        }
    }

    #[test]
    fn an_ack_survives_the_wire_and_a_refusal_carries_its_reason() {
        for ack in [
            Ack::accepted(),
            Ack::refused(&Refusal {
                message: "another release".to_owned(),
                retire: true,
            }),
        ] {
            let line = serde_json::to_string(&ack).expect("serialize");
            assert!(!line.contains('\n'), "the ack is one line: {line}");
            assert_eq!(
                serde_json::from_str::<Ack>(&line).expect("deserialize"),
                ack
            );
        }
        let refused = Ack::refused(&Refusal {
            message: "why".to_owned(),
            retire: false,
        });
        assert_eq!(refused.error.as_deref(), Some("why"));
        assert!(!refused.ok && !refused.retiring);
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
