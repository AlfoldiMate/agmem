//! Command-line and environment configuration.
//!
//! Every knob from `docs/design.md` §6. Flags win over env vars (clap `env`
//! feature handles the fallback).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use agmem_core::SpaceName;
use anyhow::{Context, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
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

    /// Root username for a remote --db server. Embedded engines have no
    /// signin, so this only applies when --db names a ws://, wss://,
    /// http:// or https:// endpoint.
    #[arg(long, env = "AGMEM_DB_USER", value_name = "USER")]
    pub db_user: Option<String>,

    /// Root password for a remote --db server; pair of --db-user.
    #[arg(
        long,
        env = "AGMEM_DB_PASS",
        value_name = "PASS",
        hide_env_values = true
    )]
    pub db_pass: Option<String>,

    /// Space (project scope) served by this instance. Unset, it is derived:
    /// the enclosing git project's name — every worktree of a repo maps to
    /// the same space — else the current directory's name, else `default`.
    #[arg(long, env = "AGMEM_SPACE")]
    pub space: Option<SpaceName>,

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

    /// This daemon replaces one that retired for its release, so its ready
    /// line can say that sessions still on the old one need a restart
    /// (issue #112). Passed by the session that respawned it; meaningless
    /// without --daemon-serve.
    #[arg(long, hide = true, requires = "daemon_serve")]
    pub took_over: bool,

    /// Seconds the shared daemon stays up with no sessions attached. 0 keeps
    /// it until the machine restarts.
    #[arg(
        long,
        env = "AGMEM_IDLE_TIMEOUT",
        default_value_t = 600,
        value_name = "SECONDS"
    )]
    pub idle_timeout: u64,

    /// One-shot mode instead of serving MCP.
    #[command(subcommand)]
    pub command: Option<CliCommand>,
}

/// A run that does one thing, prints it, and exits — the shell-facing surface
/// (issue #46), as opposed to the MCP one the flags above configure.
#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum CliCommand {
    /// Print the session-start context block to stdout and exit.
    ///
    /// Same output and semantics as the MCP `context` tool, so a shell hook
    /// can inject the briefing into a session instead of hoping the model
    /// asks for it.
    Context(ContextArgs),

    /// Answer a Claude Code hook event: read the payload on stdin, print the
    /// reply, exit 0. The shell side of the agmem plugin (`plugin/`).
    Hook(HookArgs),
}

/// Which event `agmem hook` is answering.
#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct HookArgs {
    #[command(subcommand)]
    pub event: crate::hook::HookEvent,
}

/// What `agmem context` passes through to the `context` tool, one flag per
/// tool parameter.
#[derive(Debug, Clone, Default, PartialEq, Eq, Args)]
pub struct ContextArgs {
    /// What the session is about; aims the Relevant section. Omit for a
    /// general orientation.
    #[arg(long)]
    pub query: Option<String>,

    /// Where to look: `current`, `user`, `all`, or a space name. Defaults to
    /// `current` and `user` together. (The project `current` means comes from
    /// the top-level --space / AGMEM_SPACE, derived from the cwd when unset.)
    #[arg(long)]
    pub space: Option<String>,

    /// How many characters the block may take, 6000 by default.
    #[arg(long, value_name = "N")]
    pub budget_chars: Option<u32>,
}

/// Embedding backend selector (`docs/design.md` §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EmbedderKind {
    /// fastembed/ONNX local model (default).
    Fastembed,
    /// No embeddings at all. Not a supported deployment: agmem is BM25 plus a
    /// local model, always. Hidden from `--help`; it exists so the subprocess
    /// tests can start the real binary where CI forbids a model download.
    #[value(hide = true)]
    None,
}

impl EmbedderKind {
    /// The spelling `--embedder` takes, which is also what crosses the daemon
    /// handshake and what a freshly spawned daemon is started with.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fastembed => "fastembed",
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
                    "{key} names no agmem tool. The seven are: {}.",
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
    /// Root signin presented to a remote server; `None` on embedded engines.
    pub db_user: Option<String>,
    pub db_pass: Option<String>,
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
    pub took_over: bool,
    pub idle_timeout: u64,
    /// The one-shot subcommand this run is, if it is one.
    pub command: Option<CliCommand>,
}

/// The space served when nothing names one (issue #44): the enclosing git
/// project, else the directory itself, else [`fallback_space`].
///
/// Derived here, in the session process, so the daemon handshake carries a
/// per-project space even under one global MCP registration — static client
/// config cannot vary by folder, but the cwd the client launches us in can.
fn derived_space(cwd: &Path) -> SpaceName {
    project_dir(cwd)
        .as_deref()
        .and_then(space_from_dir_name)
        .or_else(|| space_from_dir_name(cwd))
        .unwrap_or_else(fallback_space)
}

/// The space of last resort, kept as the literal `default` so stores written
/// before derivation existed keep resolving to the same place.
fn fallback_space() -> SpaceName {
    SpaceName::new("default").expect("a valid slug")
}

/// The directory that names the project around `start`: the parent of the git
/// *common* dir. That is the repo root for a plain checkout, and the shared
/// project root for a linked worktree — bare `.bare/`-style layouts included —
/// so worktrees share one space rather than getting one per branch.
fn project_dir(start: &Path) -> Option<PathBuf> {
    let dot_git = start
        .ancestors()
        .map(|dir| dir.join(".git"))
        .find(|candidate| candidate.exists())?;
    let git_dir = if dot_git.is_dir() {
        dot_git
    } else {
        // A worktree's `.git` is one line: `gitdir: <its private dir>`.
        let target = std::fs::read_to_string(&dot_git).ok()?;
        let target = PathBuf::from(target.strip_prefix("gitdir:")?.trim());
        // join() discards its base when the target is already absolute
        dot_git.parent()?.join(target)
    };
    // A linked worktree's private dir names the shared dir in `commondir`,
    // usually relatively; a primary checkout has no such file.
    let common = match std::fs::read_to_string(git_dir.join("commondir")) {
        Ok(shared) => git_dir.join(shared.trim()),
        Err(_) => git_dir,
    };
    // canonicalize collapses the `../..` a relative commondir carries
    std::fs::canonicalize(common)
        .ok()?
        .parent()
        .map(Path::to_path_buf)
}

/// `dir`'s name as a space: lowercased, anything outside `[a-z0-9-_]` becomes
/// `-`, capped at [`SpaceName::MAX_LEN`]. `None` when nothing survives — and
/// for the reserved `user` space, because a project that happens to be named
/// `user` must not absorb cross-project personal memory.
fn space_from_dir_name(dir: &Path) -> Option<SpaceName> {
    let name = dir.file_name()?.to_string_lossy();
    let slug: String = name
        .chars()
        .map(|c| match c {
            _ if c.is_ascii_alphanumeric() => c.to_ascii_lowercase(),
            '-' | '_' => c,
            _ => '-',
        })
        .take(SpaceName::MAX_LEN)
        .collect();
    let slug = slug.trim_matches('-');
    if slug.is_empty() || slug == SpaceName::user().as_str() {
        return None;
    }
    SpaceName::new(slug).ok()
}

/// True when `url` names a DB in another process — no local lock file needed.
fn is_remote(url: &str) -> bool {
    ["ws://", "wss://", "http://", "https://"]
        .iter()
        .any(|scheme| url.starts_with(scheme))
}

impl Config {
    /// True when the DB lives in another process — no local lock file needed.
    pub fn db_is_remote(&self) -> bool {
        is_remote(&self.db_url)
    }

    /// The root signin to present to a remote server, when this deployment
    /// set one. Embedded engines have no signin, so never any credentials.
    pub fn db_credentials(&self) -> Option<agmem_store::db::Credentials<'_>> {
        match (&self.db_user, &self.db_pass) {
            (Some(user), Some(pass)) if self.db_is_remote() => {
                Some(agmem_store::db::Credentials { user, pass })
            }
            _ => None,
        }
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
        if is_remote(&db_url) && (self.db_user.is_some() != self.db_pass.is_some()) {
            bail!(
                "AGMEM_DB_USER and AGMEM_DB_PASS come as a pair: one without the \
                 other would reach the server unauthenticated and be refused there. \
                 Set both or neither."
            );
        }
        let space = match self.space {
            Some(space) => space,
            // An unreadable cwd is just the far end of the derivation cascade,
            // not a reason to refuse to serve memory.
            None => std::env::current_dir()
                .map(|cwd| derived_space(&cwd))
                .unwrap_or_else(|_| fallback_space()),
        };
        Ok(Config {
            data_dir,
            db_url,
            db_user: self.db_user,
            db_pass: self.db_pass,
            space,
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
            took_over: self.took_over,
            idle_timeout: self.idle_timeout,
            command: self.command,
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
    fn half_a_credential_pair_is_refused_for_remote_engines() {
        let err =
            Cli::try_parse_from(["agmem", "--db", "ws://localhost:8000", "--db-user", "root"])
                .expect("parse")
                .resolve()
                .expect_err("one credential without the other");
        assert!(err.to_string().contains("pair"), "{err}");

        let cfg = parse(&[
            "--db",
            "ws://localhost:8000",
            "--db-user",
            "root",
            "--db-pass",
            "s3cret",
        ]);
        assert!(cfg.db_credentials().is_some());

        // Embedded engines have no signin; stray credentials never apply.
        let cfg = parse(&["--db-user", "root", "--data", "/tmp/agmem-test"]);
        assert!(cfg.db_credentials().is_none());
    }

    #[test]
    fn invalid_space_is_rejected_at_parse_time() {
        let err = Cli::try_parse_from(["agmem", "--space", "Not A Slug"]);
        assert!(err.is_err());
    }

    #[test]
    fn an_explicit_space_wins_over_derivation() {
        let cfg = parse(&["--space", "pinned", "--data", "/tmp/agmem-test"]);
        assert_eq!(cfg.space.as_str(), "pinned");
    }

    #[test]
    fn a_run_with_no_subcommand_is_the_server() {
        let cfg = parse(&["--data", "/tmp/agmem-test"]);
        assert_eq!(cfg.command, None);
    }

    #[test]
    fn the_context_subcommand_carries_the_tool_parameters() {
        let cfg = parse(&[
            "--data",
            "/tmp/agmem-test",
            "context",
            "--query",
            "release work",
            "--space",
            "all",
            "--budget-chars",
            "500",
        ]);
        assert_eq!(
            cfg.command,
            Some(CliCommand::Context(ContextArgs {
                query: Some("release work".to_owned()),
                space: Some("all".to_owned()),
                budget_chars: Some(500),
            }))
        );
    }

    #[test]
    fn the_subcommands_space_selects_scope_and_the_flags_name_the_project() {
        // Two different `--space`s on purpose: before the subcommand it is the
        // project (what `current` resolves to), after it the tool's scope.
        let cfg = parse(&["--space", "pinned", "--data", "/tmp/agmem-test", "context"]);
        assert_eq!(cfg.space.as_str(), "pinned");
        assert_eq!(
            cfg.command,
            Some(CliCommand::Context(ContextArgs::default()))
        );
    }

    /// A directory tree to derive spaces from, torn down on drop.
    fn root() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    fn derive(from: &std::path::Path) -> String {
        derived_space(from).as_str().to_owned()
    }

    #[test]
    fn space_derives_from_the_repo_root_not_the_subdir() {
        let root = root();
        let deep = root.path().join("Alpha/src/deep");
        std::fs::create_dir_all(&deep).expect("dirs");
        std::fs::create_dir(root.path().join("Alpha/.git")).expect(".git");
        assert_eq!(derive(&deep), "alpha");
    }

    #[test]
    fn a_linked_worktree_lands_in_its_repos_space() {
        let root = root();
        // What `git worktree add ../alpha-fix` leaves behind: a private dir
        // under the repo's .git whose `commondir` points back at it, and a
        // one-line `.git` file in the worktree naming that private dir.
        let private = root.path().join("Alpha/.git/worktrees/fix");
        std::fs::create_dir_all(&private).expect("dirs");
        std::fs::write(private.join("commondir"), "../..\n").expect("commondir");
        let worktree = root.path().join("alpha-fix");
        std::fs::create_dir(&worktree).expect("worktree");
        std::fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", private.display()),
        )
        .expect(".git file");
        assert_eq!(derive(&worktree), "alpha");
    }

    #[test]
    fn a_bare_layout_names_the_project_root() {
        let root = root();
        // The .bare-plus-sibling-worktrees layout: the common dir is
        // <project>/.bare, so every worktree derives the project's name.
        let private = root.path().join("Proj/.bare/worktrees/main");
        std::fs::create_dir_all(&private).expect("dirs");
        std::fs::write(private.join("commondir"), "../..\n").expect("commondir");
        let worktree = root.path().join("Proj/main");
        std::fs::create_dir(&worktree).expect("worktree");
        std::fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", private.display()),
        )
        .expect(".git file");
        assert_eq!(derive(&worktree), "proj");
    }

    #[test]
    fn no_repo_falls_back_to_the_directory_name_slugged() {
        let root = root();
        let dir = root.path().join("My Project!");
        std::fs::create_dir(&dir).expect("dir");
        assert_eq!(derive(&dir), "my-project");
    }

    #[test]
    fn the_reserved_user_space_is_never_derived() {
        let root = root();
        let repo = root.path().join("user");
        std::fs::create_dir_all(repo.join(".git")).expect("dirs");
        assert_eq!(derive(&repo), "default");
    }

    #[test]
    fn a_name_with_nothing_usable_falls_back_to_default() {
        let root = root();
        let dir = root.path().join("…—…");
        std::fs::create_dir(&dir).expect("dir");
        assert_eq!(derive(&dir), "default");
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
        .expect("every name is a known tool");

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
