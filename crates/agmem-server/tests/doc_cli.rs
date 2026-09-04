//! `agmem doc` where no daemon applies: the direct route (#135).
//!
//! `--no-daemon` on an embedded store takes the same path non-Unix platforms
//! take — open the store here, call the tool, print — and, unlike `mem://`,
//! keeps what one call wrote for the next one. Each verb runs as the built
//! binary, one process per call, the way a shell runs it: an embedded store
//! releases its lock when the process that opened it exits, so two direct
//! calls in one process would contend for it — which is a test's shape, not
//! a user's. The daemon route is covered where the daemon's own harness
//! lives, in `tests/daemon.rs`.

use std::io::Write as _;
use std::process::{Command, Stdio};

use agmem_core::DocKind;
use agmem_server::config::{Cli, CliCommand, DocForgetArgs, DocGetArgs, DocListArgs, DocVerb};
use clap::Parser as _;
use serde_json::Value;

/// What one `agmem --no-daemon … doc <tail>` process printed and how it
/// ended: `Ok(stdout)` on exit 0, else `Err(stderr)`.
fn run(data: &tempfile::TempDir, tail: &[&str], stdin: &str) -> Result<String, String> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_agmem"))
        .args(["--data"])
        .arg(data.path())
        .args([
            "--space",
            "fresh",
            "--embedder",
            "none",
            "--no-daemon",
            "doc",
        ])
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

fn put(data: &tempfile::TempDir, tail: &[&str], content: &str) -> Result<String, String> {
    run(data, tail, content)
}

fn get(data: &tempfile::TempDir, tail: &[&str]) -> Result<String, String> {
    run(data, tail, "")
}

fn list(data: &tempfile::TempDir, tail: &[&str]) -> String {
    run(data, tail, "").expect("list")
}

fn forget(data: &tempfile::TempDir, tail: &[&str]) -> Result<String, String> {
    run(data, tail, "")
}
/// `n` characters of markdown with paragraph breaks, so it chunks.
fn markdown(n: usize) -> String {
    let mut text = String::from("# The plan\n\n");
    let mut paragraph = 0;
    while text.chars().count() < n {
        text.push_str(&format!(
            "Paragraph {paragraph}: {}\n\n",
            "words ".repeat(120)
        ));
        paragraph += 1;
    }
    text.chars().take(n).collect()
}

#[test]
fn a_large_document_round_trips_by_title_and_by_id() {
    let data = tempfile::tempdir().expect("tempdir");
    let content = markdown(90_000);

    let line = put(
        &data,
        &[
            "put", "--title", "plan-x", "--kind", "plan", "--tag", "phase-9",
        ],
        &content,
    )
    .expect("put");
    let (id, uri) = line
        .trim_end()
        .split_once(' ')
        .expect("`<id> <uri>` on one line");
    assert_eq!(uri, format!("memory://fresh/doc/{id}"), "{line}");
    assert!(line.ends_with('\n'));

    let by_title = get(&data, &["get", "--raw", "plan-x"]).expect("get");
    assert_eq!(by_title, content, "the content comes back as stored, whole");
    let by_id = get(&data, &["get", "--raw", id]).expect("get");
    assert_eq!(by_id, content);

    // Without --raw the answer is the tool's, so a caller sees what it is.
    let shown = get(&data, &["get", "plan-x"]).expect("get");
    let shown: Value = serde_json::from_str(&shown).expect("json");
    assert_eq!(shown["found"]["episode"]["title"], "plan-x", "{shown}");
    assert_eq!(shown["found"]["episode"]["doc_kind"], "plan");
    assert_eq!(shown["found"]["episode"]["mime"], "text/markdown");
    assert_eq!(shown["found"]["versions"].as_array().map(Vec::len), Some(1));

    // A window is honoured in characters.
    let window = get(
        &data,
        &["get", "--raw", "--offset", "2", "--limit", "8", "plan-x"],
    )
    .expect("get");
    assert_eq!(window, "The plan");
}

#[test]
fn the_write_cap_is_the_tools_and_the_message_names_it() {
    let data = tempfile::tempdir().expect("tempdir");
    let error = put(
        &data,
        &["put", "--title", "too-big", "--kind", "report"],
        &markdown(100_001),
    )
    .expect_err("one over the cap");
    assert!(error.to_string().contains("100000"), "{error}");
    assert_eq!(list(&data, &["list"]), "", "nothing landed");
}

#[test]
fn a_listing_is_one_line_per_document_and_filters_by_kind() {
    let data = tempfile::tempdir().expect("tempdir");
    let plan = put(
        &data,
        &["put", "--title", "plan-a", "--kind", "plan"],
        "the plan",
    )
    .expect("put");
    let review = put(
        &data,
        &[
            "put", "--title", "review-a", "--kind", "review", "--tag", "pr-1",
        ],
        "the review",
    )
    .expect("put");
    let plan_id = plan.split(' ').next().expect("id");
    let review_id = review.split(' ').next().expect("id");

    let lines: Vec<String> = list(&data, &["list"]).lines().map(str::to_owned).collect();
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(
        lines[0].starts_with(review_id) && lines[1].starts_with(plan_id),
        "newest first, the id first on the line: {lines:?}"
    );
    assert!(lines[0].contains("review-a") && lines[0].contains("review"));

    let plans = list(&data, &["list", "--kind", "plan"]);
    assert!(
        plans.starts_with(plan_id) && plans.lines().count() == 1,
        "{plans}"
    );
    let tagged = list(&data, &["list", "--tag", "pr-1"]);
    assert!(
        tagged.starts_with(review_id) && tagged.lines().count() == 1,
        "{tagged}"
    );

    let json = list(&data, &["list", "--json"]);
    let json: Value = serde_json::from_str(&json).expect("json");
    assert_eq!(json["found"]["documents"].as_array().map(Vec::len), Some(2));
}

#[test]
fn forgetting_purges_on_request_and_a_miss_is_an_error() {
    let data = tempfile::tempdir().expect("tempdir");
    let line = put(
        &data,
        &["put", "--title", "plan-a", "--kind", "plan"],
        "the plan",
    )
    .expect("put");
    let id = line.split(' ').next().expect("id");

    let purged = forget(&data, &["forget", "--purge", id]).expect("forget");
    assert!(purged.starts_with("purged 1 record(s)"), "{purged}");
    assert!(purged.contains(id), "{purged}");
    assert_eq!(list(&data, &["list"]), "");

    let missing =
        get(&data, &["get", "--raw", "plan-a"]).expect_err("purged, so the title names nothing");
    assert!(missing.to_string().contains("plan-a"), "{missing}");

    let again = forget(&data, &["forget", "--purge", id]).expect_err("an id that names nothing");
    assert!(again.to_string().contains(id), "{again}");
}

#[test]
fn cascade_without_purge_is_refused_at_parse_time() {
    let error = Cli::try_parse_from([
        "agmem",
        "doc",
        "forget",
        "--cascade",
        "01ARZ3NDEKTSV4RRFFQ69G5FAV",
    ])
    .expect_err("cascade only means something under purge");
    assert!(error.to_string().contains("--purge"), "{error}");

    let error = Cli::try_parse_from(["agmem", "doc", "put", "--title", "t", "--kind", "memo"])
        .expect_err("not a kind");
    assert!(error.to_string().contains("plan"), "{error}");
}

#[test]
fn the_verbs_parse_to_the_tool_parameters() {
    let cfg = Cli::try_parse_from([
        "agmem", "doc", "get", "--offset", "10", "--limit", "5", "--raw", "--space", "user",
        "plan-x",
    ])
    .expect("parse")
    .resolve()
    .expect("resolve");
    assert_eq!(
        cfg.command,
        Some(CliCommand::Doc(agmem_server::config::DocArgs {
            verb: DocVerb::Get(DocGetArgs {
                reference: "plan-x".to_owned(),
                offset: Some(10),
                limit: Some(5),
                raw: true,
                space: Some("user".to_owned()),
            })
        }))
    );

    let cfg = Cli::try_parse_from(["agmem", "doc", "list", "--kind", "plan", "--kind", "probe"])
        .expect("parse")
        .resolve()
        .expect("resolve");
    let Some(CliCommand::Doc(args)) = cfg.command else {
        panic!("doc")
    };
    assert_eq!(
        args.verb,
        DocVerb::List(DocListArgs {
            kinds: vec![DocKind::Plan, DocKind::Probe],
            tags: Vec::new(),
            space: None,
            json: false,
        })
    );

    let cfg = Cli::try_parse_from(["agmem", "doc", "forget", "--purge", "--cascade", "x"])
        .expect("parse")
        .resolve()
        .expect("resolve");
    let Some(CliCommand::Doc(args)) = cfg.command else {
        panic!("doc")
    };
    assert_eq!(
        args.verb,
        DocVerb::Forget(DocForgetArgs {
            id: "x".to_owned(),
            purge: true,
            cascade: true,
            space: None,
        })
    );
}
