//! What the daemon costs the machine, written to its log at startup and once
//! a day (issue #196).
//!
//! RSS is the wrong number on macOS: a daemon idle for days has most of its
//! pages compressed or swapped, and reported 77 MB of RSS against a 3.4 GB
//! footprint. The footprint — what Activity Monitor's Memory column and
//! `footprint(1)` show — counts those pages too. Linux has no such figure,
//! so there it is resident plus swapped.

/// This process's footprint in bytes, when the platform says.
#[must_use]
pub fn current() -> Option<u64> {
    imp::current()
}

#[cfg(target_os = "macos")]
mod imp {
    use libproc::pid_rusage::{RUsageInfoV2, pidrusage};

    pub fn current() -> Option<u64> {
        let pid = i32::try_from(std::process::id()).ok()?;
        pidrusage::<RUsageInfoV2>(pid)
            .ok()
            .map(|usage| usage.ri_phys_footprint)
    }
}

#[cfg(target_os = "linux")]
mod imp {
    pub fn current() -> Option<u64> {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        let kib = |field: &str| {
            status
                .lines()
                .find_map(|line| line.strip_prefix(field))
                .and_then(|rest| {
                    rest.trim()
                        .trim_end_matches("kB")
                        .trim()
                        .parse::<u64>()
                        .ok()
                })
        };
        Some((kib("VmRSS:")? + kib("VmSwap:").unwrap_or(0)) * 1024)
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod imp {
    pub fn current() -> Option<u64> {
        None
    }
}

/// Log the footprint, or that it cannot be read here.
pub fn log() {
    match current() {
        Some(bytes) => tracing::info!(footprint_mb = bytes / 1_000_000, "memory footprint"),
        None => tracing::debug!("memory footprint unavailable on this platform"),
    }
}

#[cfg(test)]
mod tests {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn a_running_process_has_a_footprint() {
        let bytes = super::current().expect("readable on this platform");
        assert!(
            bytes > 1_000_000,
            "a test binary is more than a megabyte: {bytes}"
        );
    }
}
