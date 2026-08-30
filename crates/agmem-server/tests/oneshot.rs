//! `agmem context` where no daemon applies: the direct route (issue #46).
//!
//! `mem://` opts out of the daemon by definition, so these run the same code
//! path `--no-daemon` and non-Unix platforms take — open the store here, call
//! the tool, hand back the block. The daemon route is covered where the
//! daemon's own harness already lives, in `tests/daemon.rs`.

use agmem_server::config::{Cli, CliCommand, Config, ContextArgs};
use agmem_server::oneshot;
use clap::Parser as _;

/// The configuration `agmem --db mem:// … context <flags>` resolves to, and
/// the arguments the subcommand carried.
fn parse(data: &tempfile::TempDir, tail: &[&str]) -> (Config, ContextArgs) {
    let data = data.path().display().to_string();
    let head = [
        "agmem",
        "--data",
        &data,
        "--db",
        "mem://",
        "--space",
        "fresh",
        "--embedder",
        "none",
        "context",
    ];
    let cfg = Cli::try_parse_from(head.iter().copied().chain(tail.iter().copied()))
        .expect("parse")
        .resolve()
        .expect("resolve");
    let Some(CliCommand::Context(args)) = cfg.command.clone() else {
        panic!("the context subcommand parsed");
    };
    (cfg, args)
}

#[tokio::test]
async fn an_empty_store_answers_with_the_block_not_an_error() {
    let data = tempfile::tempdir().expect("tempdir");
    let (cfg, args) = parse(&data, &[]);

    let block = oneshot::fetch(&cfg, args)
        .await
        .expect("mem:// takes the direct route");

    assert!(
        block.starts_with("# Memory context (spaces: fresh + user)"),
        "{block}"
    );
    assert!(block.contains("Nothing stored"), "{block}");
}

#[tokio::test]
async fn the_tools_refusal_comes_back_as_the_commands_error() {
    let data = tempfile::tempdir().expect("tempdir");
    let (cfg, args) = parse(&data, &["--budget-chars", "10"]);

    let error = oneshot::fetch(&cfg, args)
        .await
        .expect_err("a budget under the tool's floor");

    assert!(error.to_string().contains("budget_chars"), "{error}");
}
