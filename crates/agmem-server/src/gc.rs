//! `agmem gc`: delete the model weights nothing loads (issue #196).
//!
//! The model dir is a Hugging Face cache (`models--owner--name/{blobs,refs,
//! snapshots}`), and it only ever grows: a model change, a moved pin, or the
//! ONNX and fastembed runtimes from before v0.3 each left gigabytes behind
//! that no release since reads. What the configured model needs is one
//! snapshot of one repo and the blobs its files point at; everything else
//! under `models/` goes, along with the `models--*` caches fastembed wrote at
//! the data dir's root.
//!
//! A model dir named by `AGMEM_MODEL_DIR` is left alone: it may be the hub
//! cache other tools share (`model::fetch` keeps that layout on purpose), and
//! their downloads are not agmem's to delete.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use agmem_embed::model::{MODEL_DIR_ENV, Source};

use crate::config::{Config, EmbedderKind, GcArgs};

/// What to delete and why the rest stays.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// Paths to remove, each with the bytes it holds.
    pub remove: Vec<(PathBuf, u64)>,
    /// The snapshot the configured model loads from, when there is one.
    pub keep: Option<PathBuf>,
    /// Why the model dir was not looked at, when it was not.
    pub skipped: Option<String>,
}

impl Plan {
    /// Bytes the plan frees.
    #[must_use]
    pub fn bytes(&self) -> u64 {
        self.remove.iter().map(|(_, bytes)| bytes).sum()
    }
}

/// Run `agmem gc`: print the plan, then carry it out unless `--dry-run`.
///
/// Safe beside a running daemon: the file it has loaded is the configured
/// model's, which stays, and an unlinked file stays readable to whoever
/// holds it open.
///
/// # Errors
/// When a path the plan names cannot be removed.
#[allow(clippy::print_stdout)]
pub fn run(cfg: &Config, args: &GcArgs) -> anyhow::Result<()> {
    let plan = plan(cfg);
    let mut out = String::new();
    if let Some(reason) = &plan.skipped {
        let _ = writeln!(out, "models: {reason}");
    }
    if let Some(keep) = &plan.keep {
        let _ = writeln!(out, "keep    {}", keep.display());
    }
    for (path, bytes) in &plan.remove {
        let verb = if args.dry_run {
            "would remove"
        } else {
            "remove"
        };
        let _ = writeln!(out, "{verb}  {:>9}  {}", human(*bytes), path.display());
    }
    if !args.dry_run {
        apply(&plan)?;
    }
    let total = human(plan.bytes());
    let _ = match (plan.remove.is_empty(), args.dry_run) {
        (true, _) => writeln!(out, "nothing to remove"),
        (false, true) => writeln!(
            out,
            "{total} would be freed; run without --dry-run to free it"
        ),
        (false, false) => writeln!(out, "freed {total}"),
    };
    print!("{out}");
    Ok(())
}

/// What `gc` would remove under `cfg`, without touching anything.
#[must_use]
pub fn plan(cfg: &Config) -> Plan {
    let keep = match cfg.embedder {
        EmbedderKind::Llama => {
            let Source::Gguf { repo, revision, .. } = cfg.model.into_embed().spec().source;
            Some((repo, revision))
        }
        // No local model in use: every downloaded one is unused.
        EmbedderKind::Api | EmbedderKind::None => None,
    };
    let mut plan = if std::env::var_os(MODEL_DIR_ENV).is_some() {
        Plan {
            skipped: Some(format!(
                "{MODEL_DIR_ENV} names the model dir, which may be shared; left alone"
            )),
            ..Plan::default()
        }
    } else {
        plan_models(&cfg.data_dir.join("models"), keep)
    };
    plan.remove.extend(root_leftovers(&cfg.data_dir));
    plan
}

/// Delete what `plan` names.
///
/// # Errors
/// The first path that cannot be removed; what came before it is gone.
pub fn apply(plan: &Plan) -> anyhow::Result<()> {
    use anyhow::Context as _;
    for (path, _) in &plan.remove {
        let meta = std::fs::symlink_metadata(path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        if meta.is_dir() {
            std::fs::remove_dir_all(path)
        } else {
            std::fs::remove_file(path)
        }
        .with_context(|| format!("cannot remove {}", path.display()))?;
    }
    Ok(())
}

/// The cache directory name the hub gives `owner/name`.
fn repo_dir_name(repo: &str) -> String {
    format!("models--{}", repo.replace('/', "--"))
}

/// Everything under `models` but the one snapshot `keep` names and the blobs
/// its files link to.
fn plan_models(models: &Path, keep: Option<(&str, &str)>) -> Plan {
    let mut plan = Plan::default();
    let keep_dir = keep.map(|(repo, _)| repo_dir_name(repo));
    for entry in entries(models) {
        let name = entry.file_name().and_then(|n| n.to_str()).unwrap_or("");
        // The hub client's `.locks`, Finder's `.DS_Store`: bytes, and not ours.
        if name.starts_with('.') {
            continue;
        }
        if keep_dir.as_deref() == Some(name) {
            let (_, revision) = keep.expect("keep_dir implies keep");
            plan_repo(&entry, revision, &mut plan);
        } else {
            plan.remove.push((entry.clone(), size(&entry)));
        }
    }
    plan
}

/// Inside the kept repo: other snapshots, and blobs no kept file points at.
fn plan_repo(repo: &Path, revision: &str, plan: &mut Plan) {
    let snapshot = repo.join("snapshots").join(revision);
    let mut wanted = Vec::new();
    for file in entries(&snapshot) {
        if let Ok(target) = std::fs::canonicalize(&file) {
            wanted.push(target);
        }
    }
    for other in entries(&repo.join("snapshots")) {
        if other != snapshot {
            plan.remove.push((other.clone(), size(&other)));
        }
    }
    for blob in entries(&repo.join("blobs")) {
        // A download in flight is a `<blob>.lock` beside its blob.
        if blob.extension().is_some_and(|ext| ext == "lock") {
            continue;
        }
        let canonical = std::fs::canonicalize(&blob).unwrap_or_else(|_| blob.clone());
        if !wanted.contains(&canonical) {
            plan.remove.push((blob.clone(), size(&blob)));
        }
    }
    if snapshot.is_dir() {
        plan.keep = Some(snapshot);
    }
}

/// fastembed (before v0.3) cached its models in the data dir itself, beside
/// the store; nothing agmem writes there now is named like a hub repo.
fn root_leftovers(data_dir: &Path) -> Vec<(PathBuf, u64)> {
    entries(data_dir)
        .into_iter()
        .filter(|path| {
            path.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("models--"))
                && path.is_dir()
        })
        .map(|path| {
            let bytes = size(&path);
            (path, bytes)
        })
        .collect()
}

/// The entries of `dir`, sorted; none when it cannot be read.
fn entries(dir: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|read| read.filter_map(|e| e.ok().map(|e| e.path())).collect())
        .unwrap_or_default();
    paths.sort();
    paths
}

/// Bytes under `path`, symlinks counted as links, not as their targets.
fn size(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if meta.is_dir() {
        entries(path).iter().map(|child| size(child)).sum()
    } else {
        meta.len()
    }
}

/// `1.2 GB`, `318 MB`, `4 KB`.
#[allow(clippy::cast_precision_loss)]
fn human(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit >= 3 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, bytes: usize) {
        std::fs::create_dir_all(path.parent().expect("has a parent")).expect("mkdir");
        std::fs::write(path, vec![0u8; bytes]).expect("write");
    }

    #[cfg(unix)]
    fn link(target: &str, at: &Path) {
        std::fs::create_dir_all(at.parent().expect("has a parent")).expect("mkdir");
        std::os::unix::fs::symlink(target, at).expect("symlink");
    }

    #[cfg(unix)]
    #[test]
    fn only_the_pinned_snapshot_and_its_blobs_survive() {
        let data = tempfile::tempdir().expect("tempdir");
        let models = data.path().join("models");
        let repo = models.join("models--ggml-org--gemma");
        write(&repo.join("blobs/aaa"), 10);
        write(&repo.join("blobs/old"), 20);
        write(&repo.join("blobs/aaa.lock"), 0);
        write(&models.join(".DS_Store"), 6);
        link("../../blobs/aaa", &repo.join("snapshots/pin/model.gguf"));
        link("../../blobs/old", &repo.join("snapshots/stale/model.gguf"));
        write(&repo.join("refs/main"), 3);
        write(
            &models.join("models--onnx-community--gemma-ONNX/blobs/x"),
            100,
        );
        write(&models.join("coreml/cache.bin"), 50);
        write(&models.join(".locks/models--ggml-org--gemma/x.lock"), 0);
        write(&data.path().join("models--Qdrant--bge/blobs/y"), 7);
        write(&data.path().join("agmem.db/wal"), 5);

        let mut plan = plan_models(&models, Some(("ggml-org/gemma", "pin")));
        plan.remove.extend(root_leftovers(data.path()));
        let removed: Vec<String> = plan
            .remove
            .iter()
            .map(|(p, _)| {
                p.strip_prefix(data.path())
                    .expect("inside")
                    .display()
                    .to_string()
            })
            .collect();

        assert_eq!(
            removed,
            [
                "models/coreml",
                "models/models--ggml-org--gemma/snapshots/stale",
                "models/models--ggml-org--gemma/blobs/old",
                "models/models--onnx-community--gemma-ONNX",
                "models--Qdrant--bge",
            ]
        );
        assert_eq!(plan.keep, Some(repo.join("snapshots/pin")));
        assert_eq!(
            plan.bytes(),
            50 + "../../blobs/old".len() as u64 + 20 + 100 + 7,
            "a symlink counts as its own bytes, not its target's"
        );

        apply(&plan).expect("applies");
        assert!(
            repo.join("snapshots/pin/model.gguf").exists(),
            "the kept file still resolves"
        );
        assert!(!models.join("coreml").exists());
        assert!(
            data.path().join("agmem.db/wal").exists(),
            "the store is never touched"
        );
        assert_eq!(
            plan_models(&models, Some(("ggml-org/gemma", "pin"))).remove,
            []
        );
    }

    #[test]
    fn with_no_local_model_every_download_is_unused() {
        let data = tempfile::tempdir().expect("tempdir");
        let models = data.path().join("models");
        write(&models.join("models--a--b/blobs/x"), 4);
        let plan = plan_models(&models, None);
        assert_eq!(plan.remove, [(models.join("models--a--b"), 4)]);
        assert_eq!(plan.keep, None);
    }

    #[test]
    fn sizes_read_like_a_person_wrote_them() {
        assert_eq!(human(0), "0 B");
        assert_eq!(human(318_000_000), "318 MB");
        assert_eq!(human(5_300_000_000), "5.3 GB");
    }
}
