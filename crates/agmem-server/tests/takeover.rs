//! The daemon as a process, seen from the session that replaces it (issue
//! #124).
//!
//! `tests/daemon.rs` runs the daemon as a task, which cannot show the order
//! in which a *process* lets go of its two locks — the data-dir lock agmem
//! takes and the file lock the embedded store takes for itself. These start
//! the real binary and watch the edge a retiring daemon exposes to the
//! session waiting on it.

#![cfg(unix)]

use std::fs::{File, TryLockError};
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use agmem_server::config::{Cli, Config};
use agmem_server::daemon::{self, Ack, Handshake};
use agmem_server::lock;
use clap::Parser as _;

/// The session's view of a data dir: same derivation the daemon uses, so
/// the handshake names the store the daemon opened.
fn config(data: &Path) -> Config {
    Cli::try_parse_from([
        "agmem",
        "--data",
        &data.display().to_string(),
        "--embedder",
        "none",
        "--idle-timeout",
        "0",
    ])
    .expect("parse")
    .resolve()
    .expect("resolve")
}

/// The real binary, serving `data` the way a session would have started it:
/// detached streams, its log in the data dir.
fn start_daemon(data: &Path) -> Child {
    Command::new(env!("CARGO_BIN_EXE_agmem"))
        .args([
            "--daemon-serve",
            "--data",
            &data.display().to_string(),
            "--embedder",
            "none",
            "--idle-timeout",
            "0",
            "--log-file",
            &data.join(daemon::DAEMON_LOG_FILE).display().to_string(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start the daemon")
}

fn wait_for_socket(socket: &Path) {
    for _ in 0..600 {
        if UnixStream::connect(socket).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("the daemon never bound {}", socket.display());
}

/// A handshake from a release that does not exist: the daemon retires.
fn retire(socket: &Path, cfg: &Config) {
    let mut asked = Handshake::new(cfg);
    asked.release = "999.0.0".to_owned();
    let mut line = serde_json::to_vec(&asked).expect("serialize");
    line.push(b'\n');

    let mut stream = UnixStream::connect(socket).expect("connect");
    stream.write_all(&line).expect("send the handshake");
    let mut reply = String::new();
    BufReader::new(&stream)
        .read_line(&mut reply)
        .expect("read the ack");
    let ack: Ack = serde_json::from_str(&reply).expect("the ack is JSON");
    assert!(!ack.ok && ack.retiring, "{ack:?}");
}

#[test]
fn a_retiring_daemon_lets_go_of_the_store_before_the_data_dir_lock() {
    let data = tempfile::tempdir().expect("tempdir");
    let cfg = config(data.path());
    let socket = daemon::socket_path(data.path()).expect("socket path");
    let mut daemon = start_daemon(data.path());
    wait_for_socket(&socket);

    // The embedded store's own lock file, held by the daemon's process
    // while the store is open. Its existence is asserted so a renamed file
    // cannot turn the check below into a lock on nothing.
    let store_lock = data.path().join("agmem.db").join("LOCK");
    assert!(
        store_lock.exists(),
        "the store keeps its lock at {}",
        store_lock.display()
    );

    retire(&socket, &cfg);

    // The data-dir lock is what a session polls for before it starts the
    // next daemon (`wait_for_store_lock`). The first instant it is free,
    // the store must be free too — or the daemon that session starts dies
    // on it, which is what happened on every other upgrade before #124.
    let deadline = Instant::now() + Duration::from_secs(10);
    while !lock::probe(data.path()).expect("probe the data-dir lock") {
        assert!(
            Instant::now() < deadline,
            "the retiring daemon never released the data-dir lock"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let store = File::options()
        .read(true)
        .write(true)
        .open(&store_lock)
        .expect("open the store's lock file");
    match store.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => panic!(
            "the data-dir lock was released while the store was still locked: a daemon \
             started on that signal dies on the store (issue #124)"
        ),
        Err(TryLockError::Error(error)) => panic!("locking the store's lock file: {error}"),
    }

    let status = daemon.wait().expect("wait for the daemon");
    assert!(status.success(), "the retiring daemon exits cleanly: {status}");
}

#[test]
fn a_daemon_that_cannot_take_the_store_says_so_in_its_log() {
    let data = tempfile::tempdir().expect("tempdir");
    let socket = daemon::socket_path(data.path()).expect("socket path");
    let mut owner = start_daemon(data.path());
    wait_for_socket(&socket);

    // A second daemon on the same data dir: what a session starts when it
    // misjudges the first one's exit. Its stderr is closed, like every
    // daemon's; the log is the only place its reason can go.
    let status = start_daemon(data.path())
        .wait()
        .expect("wait for the second daemon");
    assert!(!status.success(), "a daemon without the store exits non-zero");

    let log = std::fs::read_to_string(data.path().join(daemon::DAEMON_LOG_FILE))
        .expect("read the daemon log");
    assert!(
        log.contains("already owns the data dir"),
        "the log names the refusal, not just 'agmem starting': {log}"
    );
    assert!(
        log.contains("stopped with an error"),
        "and marks it as the reason the daemon is gone: {log}"
    );

    owner.kill().expect("stop the owner");
    let _ = owner.wait();
}
