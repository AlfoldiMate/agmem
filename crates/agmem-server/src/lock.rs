//! Single-writer discipline for embedded stores.
//!
//! Embedded SurrealKV has no documented cross-process locking (design §1), so
//! agmem holds an exclusive advisory lock on the data dir for its lifetime.
//! Remote engines (`ws://` …) skip this — the DB server is the boundary there.

use std::fs::{self, File, TryLockError};
use std::io::Write;
use std::path::Path;

use anyhow::{Context, bail};

/// Name of the lock file inside the data dir.
pub const LOCK_FILE: &str = "agmem.lock";

/// Holds the exclusive data-dir lock; released when dropped (or on process
/// exit, including crashes — advisory locks die with the process).
#[derive(Debug)]
pub struct DataDirLock {
    _file: File,
}

/// Create the data dir if needed and take the exclusive lock.
///
/// # Errors
/// Fails when another process already holds the lock, with a message naming
/// the owning pid and both ways to share it — the daemon (issue #37) and a
/// remote `ws://` engine.
pub fn acquire(data_dir: &Path) -> anyhow::Result<DataDirLock> {
    fs::create_dir_all(data_dir)
        .with_context(|| format!("cannot create data dir {}", data_dir.display()))?;
    let path = data_dir.join(LOCK_FILE);
    let mut file = File::options()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("cannot open lock file {}", path.display()))?;

    match file.try_lock() {
        Ok(()) => {
            file.set_len(0)?;
            writeln!(file, "{}", std::process::id())?;
            Ok(DataDirLock { _file: file })
        }
        Err(TryLockError::WouldBlock) => {
            let owner = fs::read_to_string(&path).unwrap_or_default();
            let owner = owner.trim();
            bail!(
                "another agmem process (pid {owner}) already owns the data dir {}. \
                 Sessions normally share it through the daemon that pid is running, \
                 so drop --no-daemon to attach to it — or point AGMEM_DB=ws://<host> \
                 at a shared SurrealDB server.",
                data_dir.display()
            )
        }
        Err(TryLockError::Error(err)) => {
            Err(err).with_context(|| format!("cannot lock {}", path.display()))
        }
    }
}
