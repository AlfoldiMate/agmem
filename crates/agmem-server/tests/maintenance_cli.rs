//! `agmem consolidate` and `agmem forget` where no daemon applies: the
//! direct route (#150).
//!
//! The maintenance pair is off the default MCP list, so the shell is the
//! door an agent uses; these run each verb as the built binary, one process
//! per call, the way `tests/doc_cli.rs` does and for the same reason — an
//! embedded store releases its lock when the process that opened it exits.
//! The daemon route is covered in `tests/daemon.rs`.

use std::io::Write as _;
use std::process::{Command, Stdio};

use agmem_server::config::{Cli, CliCommand, ForgetArgs};
use clap::Parser as _;
use serde_json::Value;

/// Backdate a document by `days` between two agmem processes:
/// `orphan_documents` keeps a 30-day grace (#137), and `created_at` is the
/// engine's column. The embedded store releases its lock only when the
/// process that opened it exits, so this re-runs the test binary with
/// [`backdate_helper`] selected and the work described in an env var.
fn backdate(data: &tempfile::TempDir, id: &str, days: i64) {
    let status = Command::new(std::env::current_exe().expect("test binary"))
        .args(["--exact", "backdate_helper", "--nocapture"])
        .env(
            BACKDATE_ENV,
            format!("{}|{id}|{days}", data.path().join("agmem.db").display()),
        )
        .status()
        .expect("spawn the helper");
    assert!(status.success(), "the backdate helper failed");
}

/// `<db path>|<episode id>|<days>` for [`backdate_helper`].
const BACKDATE_ENV: &str = "AGMEM_TEST_BACKDATE";

/// Not a test: the child process [`backdate`] spawns. A no-op in the
/// ordinary run, where the env var is unset.
#[test]
fn backdate_helper() {
    let Some(spec) = std::env::var_os(BACKDATE_ENV) else {
        return;
    };
    let spec = spec.to_string_lossy();
    let mut parts = spec.splitn(3, '|');
    let (path, id, days) = (
        parts.next().expect("path").to_owned(),
        parts.next().expect("id").to_owned(),
        parts.next().expect("days").parse::<i64>().expect("days"),
    );
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(async {
            let db = agmem_store::db::connect(&format!("surrealkv://{path}"))
                .await
                .expect("open the store");
            db.query(
                "UPDATE type::record('episode', $id)
                 SET created_at = time::now() - duration::from_days($days)",
            )
            .bind(("id", id))
            .bind(("days", days))
            .await
            .expect("backdate")
            .check()
            .expect("statements");
        });
}

/// What one `agmem --no-daemon … <tail>` process printed and how it ended:
/// `Ok(stdout)` on exit 0, else `Err(stderr)`.
fn run(data: &tempfile::TempDir, tail: &[&str], stdin: &str) -> Result<String, String> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_agmem"))
        .args(["--data"])
        .arg(data.path())
        .args(["--space", "fresh", "--embedder", "none", "--no-daemon"])
        .args(tail)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn agmem");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(stdin.as_bytes())
        .expect("write stdin");
    let output = child.wait_with_output().expect("wait");
    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    let stderr = String::from_utf8(output.stderr).expect("utf-8 stderr");
    if output.status.success() {
        Ok(stdout)
    } else {
        Err(stderr)
    }
}

/// A document nothing cites — an orphan `consolidate` lists — and its id.
fn orphan(data: &tempfile::TempDir, title: &str) -> String {
    let line = run(
        data,
        &["doc", "put", "--title", title, "--kind", "plan"],
        "# A plan\n\nNobody has distilled this yet.\n",
    )
    .expect("put");
    line.split_once(' ').expect("`<id> <uri>`").0.to_owned()
}

#[test]
fn consolidate_lists_the_orphan_and_forget_purges_it() {
    let data = tempfile::tempdir().expect("tempdir");
    let id = orphan(&data, "plan-x");
    backdate(&data, &id, 31);

    let report = run(&data, &["consolidate"], "").expect("consolidate");
    let report: Value = serde_json::from_str(&report).expect("the tool's JSON");
    assert_eq!(report["spaces"], serde_json::json!(["fresh"]), "{report}");
    let orphans = report["orphan_documents"]
        .as_array()
        .expect("orphan_documents");
    assert_eq!(orphans.len(), 1, "{report}");
    assert_eq!(orphans[0]["episode"], format!("episode:{id}"));
    assert_eq!(orphans[0]["title"], "plan-x");

    // A dry run shows the row and moves nothing: the document still reads.
    // (Verbatim text has no window to close, so a document is purged.)
    let matched = run(
        &data,
        &["forget", "--dry-run", "--purge", &format!("episode:{id}")],
        "",
    )
    .expect("dry run");
    let matched: Value = serde_json::from_str(&matched).expect("`matched` as JSON");
    let matched = matched.as_array().expect("a list");
    assert_eq!(matched.len(), 1, "{matched:?}");
    assert_eq!(matched[0]["id"], id);
    assert_eq!(matched[0]["kind"], "episode");
    run(&data, &["doc", "get", "--raw", &id], "").expect("still stored");

    // The real call purges it, and says so the way `doc forget` does.
    let purged = run(&data, &["forget", "--purge", &format!("episode:{id}")], "").expect("forget");
    assert_eq!(purged, format!("purged 1 record(s), 1 chunk(s): {id}\n"));
    let report = run(&data, &["consolidate"], "").expect("consolidate");
    let report: Value = serde_json::from_str(&report).expect("json");
    assert_eq!(
        report["orphan_documents"].as_array().map(Vec::len),
        Some(0),
        "a purged document is no longer an orphan to tidy: {report}"
    );
}

#[test]
fn forget_purges_on_request_and_a_miss_is_an_error() {
    let data = tempfile::tempdir().expect("tempdir");
    let id = orphan(&data, "plan-y");

    let purged = run(&data, &["forget", "--purge", &id], "").expect("purge by bare id");
    assert_eq!(purged, format!("purged 1 record(s), 1 chunk(s): {id}\n"));

    let again = run(&data, &["forget", "--purge", &id], "").expect_err("names nothing now");
    assert!(again.contains("forget tool refused"), "{again}");
}

#[test]
fn forget_wants_at_least_one_id_and_cascade_wants_purge() {
    let no_ids = Cli::try_parse_from(["agmem", "forget"]).expect_err("an id is required");
    assert!(
        no_ids.to_string().contains("<ID>"),
        "by-query forgetting is the tool's, not the shell's: {no_ids}"
    );
    let cascade = Cli::try_parse_from(["agmem", "forget", "--cascade", "x"])
        .expect_err("cascade only means anything under purge");
    assert!(cascade.to_string().contains("--purge"), "{cascade}");
}

#[test]
fn the_verbs_parse_to_the_tool_parameters() {
    let cfg = Cli::try_parse_from([
        "agmem",
        "forget",
        "--purge",
        "--cascade",
        "--dry-run",
        "--space",
        "all",
        "memory:a",
        "episode:b",
    ])
    .expect("parse")
    .resolve()
    .expect("resolve");
    assert_eq!(
        cfg.command,
        Some(CliCommand::Forget(ForgetArgs {
            ids: vec!["memory:a".to_owned(), "episode:b".to_owned()],
            purge: true,
            cascade: true,
            dry_run: true,
            space: Some("all".to_owned()),
        }))
    );

    let cfg = Cli::try_parse_from(["agmem", "consolidate", "--space", "user"])
        .expect("parse")
        .resolve()
        .expect("resolve");
    match cfg.command {
        Some(CliCommand::Consolidate(args)) => assert_eq!(args.space.as_deref(), Some("user")),
        other => panic!("{other:?}"),
    }
}
