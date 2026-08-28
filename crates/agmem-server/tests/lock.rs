//! Single-writer lock: a second acquisition of the same data dir must fail
//! with an actionable message, and release must make it available again.

use agmem_server::lock;

#[test]
fn second_acquire_fails_then_release_frees() {
    let dir = tempfile::tempdir().expect("tempdir");

    let first = lock::acquire(dir.path()).expect("first acquire");

    let err = lock::acquire(dir.path()).expect_err("second acquire must fail");
    let msg = err.to_string();
    assert!(msg.contains("another agmem process"), "got: {msg}");
    assert!(
        msg.contains("ws://"),
        "must point at the sharing alternative: {msg}"
    );
    assert!(
        msg.contains(&std::process::id().to_string()),
        "must name the owning pid: {msg}"
    );

    drop(first);
    let _third = lock::acquire(dir.path()).expect("acquire after release");
}
