//! Command-line and environment configuration.
//!
//! Every knob from `docs/design.md` §6. Flags win over env vars (clap `env`
//! feature handles the fallback).

use std::collections::BTreeMap;
use std::path::PathBuf;

use agmem_core::SpaceName;
use anyhow::{Context, bail};
use clap::{Parser, ValueEnum};
use serde::{Deserialize, Serialize};

/// Prefix of the environment variables that override a tool description.
pub const TOOL_DESC_PREFIX: &str = "AGMEM_TOOL_DESC_";

/// Raw command line, before defaults are resolved.
#[derive(Debug, Parser)]
#[command(
    name = "agmem",
    version,
    about = "Agent memory over MCP — stdio server"
)]
pub struct Cli {
    /// Data directory (embedded DB file, lock file). Platform data dir by default.
    #[arg(long, env = "AGMEM_DATA", value_name = "DIR")]
    pub data: Option<PathBuf>,

    /// Database connection string: surrealkv://<path>, mem://, or ws://host:port.
    /// Defaults to surrealkv://<data>/agmem.db.
    #[arg(long, env = "AGMEM_DB", value_name = "URL")]
    pub db: Option<String>,

    /// Space (project scope) served by this instance.
    #[arg(long, env = "AGMEM_SPACE", default_value = "default")]
    pub space: SpaceName,

    /// Embedding backend.
    #[arg(long, env = "AGMEM_EMBEDDER", value_enum, default_value_t = EmbedderKind::Fastembed)]
    pub embedder: EmbedderKind,

    /// Retrieval candidate pool size.
    #[arg(long, env = "AGMEM_POOL", default_value_t = 64, value_name = "N")]
    pub pool: u16,

    /// Ceiling for recall `k`.
    #[arg(
        long = "max-k",
        env = "AGMEM_MAX_K",
        default_value_t = 50,
        value_name = "N"
    )]
    pub max_k: u16,

    /// Log filter (tracing EnvFilter syntax, e.g. "info,agmem_store=debug").
    /// Defaults to INFO for agmem and WARN for everything it depends on.
    #[arg(long, env = "AGMEM_LOG", default_value = crate::telemetry::DEFAULT_LOG)]
    pub log: String,

    /// Append logs to this file instead of stderr.
    #[arg(long, env = "AGMEM_LOG_FILE", value_name = "FILE")]
    pub log_file: Option<PathBuf>,

    /// Run the installation self-check and exit.
    #[arg(long)]
    pub doctor: bool,

    /// Re-embed the store with the configured backend and exit — the
    /// sanctioned way to change embedding model or width. No env var: a
    /// maintenance pass that rewrites every vector should not be switchable
    /// by a stray export.
    #[arg(long)]
    pub reindex: bool,

    /// Open the store in this process instead of through the shared daemon.
    /// One process can hold an embedded store, so this is the old
    /// one-session-at-a-time behaviour.
    #[arg(long, env = "AGMEM_NO_DAEMON")]
    pub no_daemon: bool,

    /// Be the shared store daemon. Started automatically by the first session
    /// that needs one; not meant to be run by hand.
    #[arg(long, hide = true)]
    pub daemon_serve: bool,

    /// Seconds the shared daemon stays up with no sessions attached. 0 keeps
    /// it until the machine restarts.
    #[arg(
        long,
        env = "AGMEM_IDLE_TIMEOUT",
        default_value_t = 600,
        value_name = "SECONDS"
    )]
    pub idle_timeout: u64,
}

/// Embedding backend selector (`docs/design.md` §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EmbedderKind {
    /// fastembed/ONNX local model (default).
    Fastembed,
    /// model2vec static embeddings (pure Rust).
    Static,
    /// No embeddings; BM25-only degraded mode.
    None,
}

impl EmbedderKind {
    /// The spelling `--embedder` takes, which is also what crosses the daemon
    /// handshake and what a freshly spawned daemon is started with.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fastembed => "fastembed",
            Self::Static => "static",
            Self::None => "none",
        }
    }
}

/// Per-tool description overrides, keyed by tool name (design §3.1).
///
/// A tool description is the whole product surface — it is what decides
/// whether an agent reaches for memory at all (design §9 risk 4) — and it is
/// the one part of agmem a deployment has a reason to reword without waiting
/// for a release. `AGMEM_TOOL_DESC_REMEMBER=…` replaces `remember`'s text
/// outright; anything not named keeps the built-in.
///
/// There is no partial form on purpose: an override is a whole description,
/// so what the agent reads is exactly what the operator wrote, rather than a
/// splice of two voices.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ToolDescriptions(BTreeMap<String, String>);

impl ToolDescriptions {
    /// Collect the overrides this process was started with.
    ///
    /// # Errors
    /// When a variable names something that is not a tool, or carries only
    /// whitespace. Both are typos with no safe reading: an unknown name would
    /// override nothing and a blank description would hand every agent an
    /// unlabelled tool, and either one silently produces a server that behaves
    /// differently from the one the operator thought they configured.
    pub fn from_env() -> anyhow::Result<Self> {
        Self::collect(std::env::vars())
    }

    /// [`Self::from_env`] over an explicit environment, which is what the
    /// tests use — `std::env` is process-global and these run in threads.
    fn collect(vars: impl Iterator<Item = (String, String)>) -> anyhow::Result<Self> {
        let mut overrides = BTreeMap::new();
        for (key, value) in vars {
            let Some(suffix) = key.strip_prefix(TOOL_DESC_PREFIX) else {
                continue;
            };
            let tool = suffix.to_ascii_lowercase();
            if !crate::tools::NAMES.contains(&tool.as_str()) {
                bail!(
                    "{key} names no agmem tool. The five are: {}.",
                    crate::tools::NAMES.join(", ")
                );
            }
            if value.trim().is_empty() {
                bail!(
                    "{key} is empty. A tool with no description is one no agent knows when to \
                     call; unset the variable to keep agmem's own wording."
                );
            }
            overrides.insert(tool, value);
        }
        Ok(Self(overrides))
    }

    /// The description to serve for `tool`, if this deployment replaced it.
    pub fn get(&self, tool: &str) -> Option<&str> {
        self.0.get(tool).map(String::as_str)
    }

    /// The tools this deployment reworded, in name order.
    pub fn tools(&self) -> impl Iterator<Item = &str> {
        self.0.keys().map(String::as_str)
    }

    /// Whether the built-in descriptions are being served unchanged.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl<K: Into<String>, V: Into<String>> FromIterator<(K, V)> for ToolDescriptions {
    /// Build a set directly, for tests and for anything embedding agmem.
    /// Unlike [`ToolDescriptions::from_env`] this validates nothing: the
    /// caller wrote the names in source, where a typo is visible.
    fn from_iter<I: IntoIterator<Item = (K, V)>>(pairs: I) -> Self {
        Self(
            pairs
                .into_iter()
                .map(|(tool, text)| (tool.into(), text.into()))
                .collect(),
        )
    }
}

/// Fully resolved configuration: all defaults applied.
///
/// Cloneable because the daemon builds one per attached session: the store is
/// shared, the space and the retrieval limits are not.
#[derive(Debug, Clone)]
pub struct Config {
    pub data_dir: PathBuf,
    pub db_url: String,
    pub space: SpaceName,
    pub embedder: EmbedderKind,
    pub pool: u16,
    pub max_k: u16,
    /// What this deployment says its tools are for, where it disagrees with
    /// the built-in wording. Read from the environment rather than a flag:
    /// there is one variable per tool and clap has no shape for that.
    pub tool_desc: ToolDescriptions,
    pub log: String,
    pub log_file: Option<PathBuf>,
    pub doctor: bool,
    pub reindex: bool,
    pub no_daemon: bool,
    pub daemon_serve: bool,
    pub idle_timeout: u64,
}

impl Config {
    /// True when the DB lives in another process — no local lock file needed.
    pub fn db_is_remote(&self) -> bool {
        ["ws://", "wss://", "http://", "https://"]
            .iter()
            .any(|scheme| self.db_url.starts_with(scheme))
    }
}

impl Cli {
    /// Apply defaults that need the environment (platform dirs, derived DB path).
    pub fn resolve(self) -> anyhow::Result<Config> {
        let data_dir = match self.data {
            Some(dir) => dir,
            None => directories::ProjectDirs::from("dev", "agmem", "agmem")
                .context("cannot determine the platform data directory; pass --data")?
                .data_dir()
                .to_path_buf(),
        };
        let db_url = self
            .db
            .unwrap_or_else(|| format!("surrealkv://{}", data_dir.join("agmem.db").display()));
        Ok(Config {
            data_dir,
            db_url,
            space: self.space,
            embedder: self.embedder,
            pool: self.pool,
            max_k: self.max_k,
            tool_desc: ToolDescriptions::from_env()?,
            log: self.log,
            log_file: self.log_file,
            doctor: self.doctor,
            reindex: self.reindex,
            no_daemon: self.no_daemon,
            daemon_serve: self.daemon_serve,
            idle_timeout: self.idle_timeout,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Config {
        Cli::try_parse_from(std::iter::once("agmem").chain(args.iter().copied()))
            .expect("parse")
            .resolve()
            .expect("resolve")
    }

    #[test]
    fn db_url_derives_from_data_dir() {
        let cfg = parse(&["--data", "/tmp/agmem-test"]);
        assert_eq!(cfg.db_url, "surrealkv:///tmp/agmem-test/agmem.db");
        assert!(!cfg.db_is_remote());
    }

    #[test]
    fn explicit_db_url_wins_and_remote_is_detected() {
        let cfg = parse(&["--db", "ws://localhost:8000"]);
        assert!(cfg.db_is_remote());
    }

    #[test]
    fn invalid_space_is_rejected_at_parse_time() {
        let err = Cli::try_parse_from(["agmem", "--space", "Not A Slug"]);
        assert!(err.is_err());
    }

    fn env(pairs: &[(&str, &str)]) -> anyhow::Result<ToolDescriptions> {
        ToolDescriptions::collect(
            pairs
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned())),
        )
    }

    #[test]
    fn an_override_is_keyed_by_the_lowercased_suffix() {
        let overrides = env(&[
            ("AGMEM_TOOL_DESC_RECALL", "Search what you were told."),
            ("AGMEM_TOOL_DESC_remember", "Write it down."),
            ("AGMEM_SPACE", "unrelated"),
            ("PATH", "/usr/bin"),
        ])
        .expect("all five names are known");

        assert_eq!(overrides.get("recall"), Some("Search what you were told."));
        assert_eq!(overrides.get("remember"), Some("Write it down."));
        assert_eq!(overrides.get("forget"), None, "unnamed tools keep theirs");
        assert_eq!(
            overrides.tools().collect::<Vec<_>>(),
            ["recall", "remember"]
        );
        assert!(
            env(&[("AGMEM_SPACE", "x")])
                .expect("no overrides")
                .is_empty()
        );
    }

    #[test]
    fn a_typo_is_refused_rather_than_ignored() {
        let unknown = env(&[("AGMEM_TOOL_DESC_RECAL", "…")]).expect_err("not a tool");
        assert!(
            unknown.to_string().contains("recall"),
            "the refusal lists the real names: {unknown}"
        );

        let blank = env(&[("AGMEM_TOOL_DESC_INSPECT", "   ")]).expect_err("blank");
        assert!(
            blank.to_string().contains("unset"),
            "the refusal says how to get the built-in back: {blank}"
        );
    }

    #[test]
    fn overrides_survive_the_daemon_handshake() {
        let sent = ToolDescriptions::from_iter([("context", "Read this first.")]);
        let line = serde_json::to_string(&sent).expect("serialize");
        assert_eq!(line, r#"{"context":"Read this first."}"#);
        assert_eq!(
            serde_json::from_str::<ToolDescriptions>(&line).expect("deserialize"),
            sent
        );
    }
}
