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

/// The pid recorded in the lock file, if a process ever took it.
///
/// Advisory: the number is whatever the last holder wrote, and it is only
/// useful in a message that also says how to check it. The holder that is
/// still alive is the interesting one, and [`probe`] answers that.
pub fn owner(data_dir: &Path) -> Option<String> {
    let owner = fs::read_to_string(data_dir.join(LOCK_FILE)).ok()?;
    let owner = owner.trim();
    (!owner.is_empty()).then(|| owner.to_owned())
}

/// Whether the data-dir lock is free right now, without keeping it.
///
/// A session that just watched a daemon retire uses this to know when that
/// daemon's *process* is gone (issue #112): the socket vanishes when the
/// daemon stops accepting, but the store lock goes only with the process,
/// and a fresh daemon started before then fails on it.
///
/// # Errors
/// When the lock file cannot be opened or locked for a reason other than
/// another process holding it.
pub fn probe(data_dir: &Path) -> anyhow::Result<bool> {
    let path = data_dir.join(LOCK_FILE);
    let file = File::options()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("cannot open lock file {}", path.display()))?;
    match file.try_lock() {
        // Dropping `file` releases what this just took.
        Ok(()) => Ok(true),
        Err(TryLockError::WouldBlock) => Ok(false),
        Err(TryLockError::Error(err)) => {
            Err(err).with_context(|| format!("cannot lock {}", path.display()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_probe_sees_a_held_lock_and_a_released_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(
            probe(dir.path()).expect("probe"),
            "nobody holds a fresh dir"
        );
        assert_eq!(owner(dir.path()), None, "nobody has written a pid");

        let held = acquire(dir.path()).expect("acquire");
        assert!(!probe(dir.path()).expect("probe"), "held by this process");
        assert_eq!(
            owner(dir.path()).as_deref(),
            Some(std::process::id().to_string().as_str()),
            "the holder's pid is on record"
        );

        drop(held);
        assert!(
            probe(dir.path()).expect("probe"),
            "the probe itself does not keep the lock, and neither does a dropped guard"
        );
    }
}
