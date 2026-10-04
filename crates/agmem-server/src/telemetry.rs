//! Logging setup. stdout is the MCP wire, so logs go to stderr or a file —
//! never stdout.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Context;
use tracing_subscriber::EnvFilter;

/// The filter applied when neither `--log` nor `AGMEM_LOG` says otherwise.
///
/// An allow-list, not a deny-list. SurrealKV logs its entire configuration at
/// INFO, so a bare `info` put eighteen lines of it through the middle of
/// `--doctor`'s report and into the client's MCP log on every start (issue
/// #35). Naming the noisy dependencies instead would go stale the next time
/// the tree grows one, so everything outside agmem is WARN — still loud enough
/// that a dependency in real trouble reaches the log.
///
/// An explicit filter replaces this outright, so `AGMEM_LOG=info` turns the
/// engine chatter back on for debugging.
pub const DEFAULT_LOG: &str =
    "warn,agmem=info,agmem_core=info,agmem_store=info,agmem_embed=info,agmem_server=info";

/// Initialise the global tracing subscriber.
pub fn init(filter: &str, log_file: Option<&Path>) -> anyhow::Result<()> {
    let filter = EnvFilter::try_new(filter)
        .with_context(|| format!("invalid log filter {filter:?} (AGMEM_LOG)"))?;
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_target(true);
    match log_file {
        Some(path) => {
            let file = RotatingFile::open(path, LOG_ROTATE_BYTES, LOG_KEEP)
                .with_context(|| format!("cannot open log file {}", path.display()))?;
            builder.with_writer(std::sync::Mutex::new(file)).init();
        }
        None => builder.with_writer(std::io::stderr).init(),
    }
    Ok(())
}

/// The size at which the log file is rotated. The daemon lives for days, and
/// a log that only grows reached 25 MB on the dogfood store (issue #196).
const LOG_ROTATE_BYTES: u64 = 5 * 1024 * 1024;

/// How many rotated files are kept beside the live one: `daemon.log.1` is
/// the newest, the oldest falls off. With the live file that bounds the log
/// at three files of [`LOG_ROTATE_BYTES`].
const LOG_KEEP: usize = 2;

/// An append-only log file that rotates itself by size.
///
/// The count starts from the size on disk, so a restarted daemon picks up
/// where the last one stopped rather than granting itself a fresh budget. A
/// daemon that is draining after a takeover keeps writing to the file it
/// opened, renamed or not; the two never write into the same live file for
/// longer than the drain.
struct RotatingFile {
    path: PathBuf,
    file: File,
    written: u64,
    limit: u64,
    keep: usize,
}

impl RotatingFile {
    fn open(path: &Path, limit: u64, keep: usize) -> std::io::Result<Self> {
        let file = Self::append(path)?;
        let written = file.metadata()?.len();
        Ok(Self {
            path: path.to_path_buf(),
            file,
            written,
            limit,
            keep,
        })
    }

    fn append(path: &Path) -> std::io::Result<File> {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
    }

    /// `daemon.log.<n>`.
    fn rotated(&self, n: usize) -> PathBuf {
        let mut name = self.path.as_os_str().to_owned();
        name.push(format!(".{n}"));
        PathBuf::from(name)
    }

    /// Shift `.1` → `.2` and so on, the oldest overwritten, the live file to
    /// `.1`, and start a new live file. With `keep` at zero the live file is
    /// simply truncated.
    fn rotate(&mut self) -> std::io::Result<()> {
        if self.keep == 0 {
            self.file = File::create(&self.path)?;
        } else {
            for n in (1..self.keep).rev() {
                let from = self.rotated(n);
                if from.exists() {
                    std::fs::rename(&from, self.rotated(n + 1))?;
                }
            }
            std::fs::rename(&self.path, self.rotated(1))?;
            self.file = Self::append(&self.path)?;
        }
        self.written = 0;
        Ok(())
    }
}

impl Write for RotatingFile {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        // A line is never split across files: the one that would cross the
        // limit goes to a fresh file. An empty file never rotates, so a line
        // longer than the limit still lands somewhere.
        if self.written > 0 && self.written + buf.len() as u64 > self.limit {
            self.rotate()?;
        }
        let n = self.file.write(buf)?;
        self.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    use tracing_subscriber::fmt::MakeWriter;

    use super::*;

    /// A writer that keeps what was logged, so a filter can be asserted on
    /// its output rather than on its directives.
    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl Capture {
        fn contents(&self) -> String {
            let bytes = self.0.lock().expect("capture buffer").clone();
            String::from_utf8(bytes).expect("log output is utf-8")
        }
    }

    impl Write for Capture {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .expect("capture buffer")
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl MakeWriter<'_> for Capture {
        type Writer = Self;

        fn make_writer(&self) -> Self::Writer {
            self.clone()
        }
    }

    /// Run `emit` under `filter`, built the way `init` builds it.
    fn logged_under(filter: &str, emit: impl FnOnce()) -> String {
        let capture = Capture::default();
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::try_new(filter).expect("filter parses"))
            .with_ansi(false)
            .with_target(true)
            .with_writer(capture.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, emit);
        capture.contents()
    }

    #[test]
    fn the_log_rotates_by_size_and_keeps_only_the_newest_files() {
        let dir = std::env::temp_dir().join(format!("agmem-rotate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("daemon.log");

        let mut log = RotatingFile::open(&path, 10, 2).expect("opens");
        for line in ["aaaaaaa\n", "bbbbbbb\n", "ccccccc\n", "ddddddd\n"] {
            log.write_all(line.as_bytes()).expect("writes");
        }
        let read = |p: &Path| std::fs::read_to_string(p).expect("readable");

        assert_eq!(read(&path), "ddddddd\n");
        assert_eq!(read(&dir.join("daemon.log.1")), "ccccccc\n");
        assert_eq!(read(&dir.join("daemon.log.2")), "bbbbbbb\n");
        assert!(!dir.join("daemon.log.3").exists(), "the oldest falls off");

        // A restart counts what is already on disk.
        drop(log);
        let mut log = RotatingFile::open(&path, 10, 2).expect("reopens");
        log.write_all(b"eeeeeee\n").expect("writes");
        assert_eq!(read(&path), "eeeeeee\n");
        assert_eq!(read(&dir.join("daemon.log.1")), "ddddddd\n");

        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn the_default_filter_keeps_agmem_at_info_and_its_dependencies_at_warn() {
        let output = logged_under(DEFAULT_LOG, || {
            tracing::info!(target: "surrealkv", "Enabling value log separation: true");
            tracing::info!(target: "surrealdb::core::kvs::ds", "Starting kvs store");
            tracing::info!(target: "agmem_store", "migrated to schema version 1");
            tracing::warn!(target: "surrealkv", "the store is in trouble");
        });

        assert!(
            !output.contains("value log separation"),
            "the engine configuring itself is not the operators business: {output}"
        );
        assert!(!output.contains("Starting kvs store"), "{output}");
        assert!(
            output.contains("migrated to schema version 1"),
            "our own INFO still has to arrive: {output}"
        );
        assert!(
            output.contains("the store is in trouble"),
            "a dependency in real trouble is still worth reading: {output}"
        );
    }

    #[test]
    fn an_explicit_filter_can_turn_the_engine_back_on() {
        let output = logged_under("info", || {
            tracing::info!(target: "surrealkv", "Enabling value log separation: true");
        });

        assert!(output.contains("value log separation"), "{output}");
    }
}
