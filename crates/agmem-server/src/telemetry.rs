//! Logging setup. stdout is the MCP wire, so logs go to stderr or a file —
//! never stdout.

use std::path::Path;

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
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .with_context(|| format!("cannot open log file {}", path.display()))?;
            builder.with_writer(std::sync::Mutex::new(file)).init();
        }
        None => builder.with_writer(std::io::stderr).init(),
    }
    Ok(())
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
