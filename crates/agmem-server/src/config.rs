//! Command-line and environment configuration.
//!
//! Every knob from `docs/design.md` §6. Flags win over env vars (clap `env`
//! feature handles the fallback).

use std::path::PathBuf;

use agmem_core::SpaceName;
use anyhow::Context;
use clap::{Parser, ValueEnum};

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
    #[arg(long, env = "AGMEM_LOG", default_value = "info")]
    pub log: String,

    /// Append logs to this file instead of stderr.
    #[arg(long, env = "AGMEM_LOG_FILE", value_name = "FILE")]
    pub log_file: Option<PathBuf>,

    /// Run the installation self-check and exit.
    #[arg(long)]
    pub doctor: bool,
}

/// Embedding backend selector (`docs/design.md` §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum EmbedderKind {
    /// fastembed/ONNX local model (default).
    Fastembed,
    /// model2vec static embeddings (pure Rust).
    Static,
    /// No embeddings; BM25-only degraded mode.
    None,
}

/// Fully resolved configuration: all defaults applied.
#[derive(Debug)]
pub struct Config {
    pub data_dir: PathBuf,
    pub db_url: String,
    pub space: SpaceName,
    pub embedder: EmbedderKind,
    pub pool: u16,
    pub max_k: u16,
    pub log: String,
    pub log_file: Option<PathBuf>,
    pub doctor: bool,
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
            log: self.log,
            log_file: self.log_file,
            doctor: self.doctor,
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
}
