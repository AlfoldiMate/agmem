//! `agmem hook <event>` — the shell side of the Claude Code plugin.
//!
//! A plugin's hooks are shell commands fed a JSON payload on stdin. Writing
//! them as scripts would make the plugin depend on a scripting runtime the
//! user has to install; the one binary every agmem user already has is this
//! one, so the hooks are subcommands of it. Each reads the payload, answers
//! on stdout in the shape the hook reference specifies, and exits 0 — a hook
//! must never break a session, so every failure here degrades to silence and
//! a log line, never to a non-zero exit.
//!
//! Three events carry memory behaviour:
//!
//! - `session-start` injects the briefing (`agmem context`, the same block
//!   the MCP tool assembles) before the first token, names the branch tag,
//!   and after a compaction lists what the session had recalled so the
//!   checkpoint that follows can cite it.
//! - `post-tool-use` keeps the per-session log — which claims `recall`
//!   returned, which `remember`/`reflect` wrote and cited — and nudges at
//!   two seams: a successful `git push`, and an answered `AskUserQuestion`.
//! - `stop` nudges once, when a session recalled memory and wrote none.
//!
//! The log is the one thing the store cannot keep (issue #86): `REINFORCE`
//! overwrites `last_accessed`, so "which rows did this session see" is
//! unanswerable server-side, and the citation step needs exactly that list.
//! It lives under the data dir as `hooks/<session_id>.jsonl`, one JSON
//! object per line, and is pruned at session start.

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use clap::Subcommand;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::config::{Config, ContextArgs};
use crate::oneshot;

/// Which hook event the payload on stdin belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Subcommand)]
pub enum HookEvent {
    /// SessionStart: print the memory briefing as `additionalContext`.
    SessionStart,
    /// PostToolUse: log recalls and writes; nudge at seams.
    PostToolUse,
    /// Stop: nudge once if the session recalled memory and wrote none.
    Stop,
}

/// Days a session log outlives its last write before session start removes it.
///
/// A week covers a resumed session; anything older belongs to a session no
/// hook will hear from again.
const LOG_RETENTION_DAYS: i64 = 7;

/// How many recalled ids a post-compaction briefing lists, newest first.
///
/// Enough to cover a working session's recalls; a cap because a session that
/// recalled hundreds of claims needs the recent ones, not all of them.
const COMPACT_RECALLED_CAP: usize = 40;

const HEADER: &str = "Durable project memory lives in agmem, not in this window. Before the first \
move, call the agmem `context` tool with a short query naming the work at hand, and treat \
the briefing as established fact — do not re-derive what it records; verify a specific \
claim only before acting on it. The `recall` tool reaches what the briefing omits; ask \
in words, not keywords. A claim that turns out to be stale is corrected with `remember` + \
`supersedes` (its id ends its line), never worked around. /agmem:checkpoint stores this \
session's durable state back.";

const FOOTER: &str = "The block above is this project's memory briefing (agmem). Treat it as \
established fact — do not re-derive what it records; verify a specific claim only \
before acting on it. The `recall` tool reaches what it omits; ask in words, not \
keywords. A claim that turns out to be stale is corrected with `remember` + `supersedes` \
(its id ends its line), never worked around. /agmem:checkpoint stores this session's \
durable state back.";

const COMPACTED: &str = "NOTE: context was compacted immediately before this. Anything not in agmem \
or on disk is gone — re-read files before assuming their contents, and do not trust \
remembered line numbers. A checkpoint now (/agmem:checkpoint) stores what the summary \
dropped while the reasons are still recoverable.";

const PUSH_NUDGE: &str = "git push succeeded — that is a checkpoint seam. Store the session's \
durable state in agmem now, as /agmem:checkpoint would: decisions with their reasons, \
corrected assumptions, branch state as fast-decay claims tagged branch:<slug>. Recall \
each topic before writing so corrections land as supersedes. Then continue, or suggest \
/clear if the task is finished.";

const DECISION_NUDGE: &str = "The answer above is a decision the user just made. Once its reason is \
clear, it is worth one claim in agmem: a `fact` with the decision and the reason, with \
`supersedes` set if it overturns a claim in the briefing. Not every answer needs storing; \
a routine choice does not, and one a future session would otherwise re-ask does.";

const STOP_NUDGE: &str = "This session recalled memory and has written none back. If it \
established anything durable — a decision with its reason, a corrected assumption, a \
gotcha that cost time — /agmem:checkpoint stores it; if it established nothing, nothing \
is the right amount to store.";

/// One line of the session log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
    /// When it happened, RFC 3339.
    at: String,
    /// `recall`, `write`, or `nudge`.
    kind: String,
    /// The tool (for `recall`/`write`) or the nudge id (for `nudge`).
    name: String,
    /// Ids returned (`recall`) or created (`write`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    ids: Vec<String>,
    /// Ids cited in `derived_from` (`write` from `reflect`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    cites: Vec<String>,
}

/// Read the payload from stdin, answer the event, print the answer, exit 0.
///
/// stdout is the hook's reply here, not the MCP wire; like `oneshot`, this
/// is a place the crate-wide print deny is lifted on purpose.
///
/// # Errors
/// Never in practice: every failure is logged and swallowed so the hook exits
/// 0. The signature keeps `main`'s dispatch uniform.
#[allow(clippy::print_stdout)]
pub async fn run(cfg: Config, event: HookEvent) -> anyhow::Result<()> {
    let raw = std::io::read_to_string(std::io::stdin()).unwrap_or_default();
    let payload: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    if let Some(reply) = respond(&cfg, event, &payload).await {
        println!("{reply}");
    }
    Ok(())
}

/// The JSON reply for `event`, or `None` when the hook has nothing to say.
///
/// Separated from [`run`] so tests can feed a payload without a stdin.
pub async fn respond(cfg: &Config, event: HookEvent, payload: &Value) -> Option<String> {
    let session = Session::from_payload(&cfg.data_dir, payload);
    let text = match event {
        HookEvent::SessionStart => session_start(cfg, &session, payload).await,
        HookEvent::PostToolUse => post_tool_use(&session, payload),
        HookEvent::Stop => stop(&session, payload),
    }?;
    let event_name = match event {
        HookEvent::SessionStart => "SessionStart",
        HookEvent::PostToolUse => "PostToolUse",
        HookEvent::Stop => "Stop",
    };
    Some(
        json!({
            "hookSpecificOutput": {
                "hookEventName": event_name,
                "additionalContext": text,
            }
        })
        .to_string(),
    )
}

// --- session log ----------------------------------------------------------

/// The per-session log and the directory it lives in.
#[derive(Debug, Clone)]
struct Session {
    dir: PathBuf,
    file: PathBuf,
}

impl Session {
    fn from_payload(data_dir: &Path, payload: &Value) -> Self {
        let id = payload
            .get("session_id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .unwrap_or("nosession");
        let dir = data_dir.join("hooks");
        let file = dir.join(format!("{}.jsonl", safe_name(id)));
        Self { dir, file }
    }

    fn append(&self, kind: &str, name: &str, ids: Vec<String>, cites: Vec<String>) {
        let entry = Entry {
            at: jiff::Timestamp::now().to_string(),
            kind: kind.to_owned(),
            name: name.to_owned(),
            ids,
            cites,
        };
        let written = fs::create_dir_all(&self.dir).and_then(|()| {
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.file)?;
            let mut line = serde_json::to_string(&entry).unwrap_or_default();
            line.push('\n');
            file.write_all(line.as_bytes())
        });
        if let Err(error) = written {
            tracing::warn!(%error, path = %self.file.display(), "hook log not written");
        }
    }

    fn entries(&self) -> Vec<Entry> {
        fs::read_to_string(&self.file)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }

    /// True the first time `id` is asked about this session, false after.
    fn first_time(&self, id: &str) -> bool {
        let seen = self
            .entries()
            .iter()
            .any(|e| e.kind == "nudge" && e.name == id);
        if !seen {
            self.append("nudge", id, Vec::new(), Vec::new());
        }
        !seen
    }

    /// Distinct recalled ids, newest first.
    fn recalled(&self) -> Vec<String> {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for entry in self.entries().iter().rev().filter(|e| e.kind == "recall") {
            for id in &entry.ids {
                if seen.insert(id.clone()) {
                    out.push(id.clone());
                }
            }
        }
        out
    }

    fn wrote(&self) -> bool {
        self.entries().iter().any(|e| e.kind == "write")
    }

    /// Remove logs untouched for [`LOG_RETENTION_DAYS`].
    fn prune(&self) {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return;
        };
        let horizon = std::time::SystemTime::now()
            - std::time::Duration::from_secs(60 * 60 * 24 * LOG_RETENTION_DAYS.unsigned_abs());
        for entry in entries.flatten() {
            let stale = entry
                .metadata()
                .and_then(|m| m.modified())
                .map(|modified| modified < horizon)
                .unwrap_or(false);
            if stale {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}

/// A session id reduced to a filename: anything outside `[A-Za-z0-9._-]`
/// becomes `-`, so a hostile payload cannot name a path.
fn safe_name(id: &str) -> String {
    let mut out: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    out.truncate(80);
    if out.is_empty() || out.chars().all(|c| c == '.') {
        out = "nosession".to_owned();
    }
    out
}

// --- events ---------------------------------------------------------------

async fn session_start(cfg: &Config, session: &Session, payload: &Value) -> Option<String> {
    session.prune();
    let cwd = payload
        .get("cwd")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let compacted = payload.get("source").and_then(Value::as_str) == Some("compact");

    let mut parts: Vec<String> = Vec::new();
    if compacted {
        let mut note = COMPACTED.to_owned();
        let recalled = session.recalled();
        if !recalled.is_empty() {
            let listed: Vec<&str> = recalled
                .iter()
                .take(COMPACT_RECALLED_CAP)
                .map(String::as_str)
                .collect();
            note.push_str(
                "\n\nClaims this session recalled before the compaction, newest first — the \
                 checkpoint can cite them in `derived_from` without a second recall: ",
            );
            note.push_str(&listed.join(", "));
        }
        parts.push(note);
    }

    let briefing = oneshot::fetch(cfg, ContextArgs::default()).await;
    let memory = match briefing {
        Ok(block) if !block.trim().is_empty() => format!("{}\n\n{FOOTER}", block.trim()),
        Ok(_) => HEADER.to_owned(),
        Err(error) => {
            tracing::warn!(%error, "briefing unavailable; falling back to the pull nudge");
            HEADER.to_owned()
        }
    };
    let branch_note = branch_of(&cwd)
        .map(|b| {
            format!(
                " In-flight state for the current branch ({b}) carries the tag branch:{} — \
                 recall with that tag when resuming work here.",
                slug(&b)
            )
        })
        .unwrap_or_default();
    parts.push(format!("{memory}{branch_note}"));
    Some(parts.join("\n\n"))
}

fn post_tool_use(session: &Session, payload: &Value) -> Option<String> {
    let tool = payload.get("tool_name").and_then(Value::as_str)?;
    let input = payload.get("tool_input").unwrap_or(&Value::Null);
    let response = payload.get("tool_response").unwrap_or(&Value::Null);

    match tool {
        "Bash" => {
            let command = input.get("command").and_then(Value::as_str).unwrap_or("");
            let ok = response
                .get("exit_code")
                .and_then(Value::as_i64)
                .unwrap_or(0)
                == 0;
            (ok && is_push(command) && session.first_time("push")).then(|| PUSH_NUDGE.to_owned())
        }
        "AskUserQuestion" => Some(DECISION_NUDGE.to_owned()),
        _ => {
            let (server, verb) = mcp_parts(tool)?;
            if !server.contains("agmem") {
                return None;
            }
            let body = response_body(response);
            match verb {
                "recall" => {
                    let ids = strings_at(&body, &["hits"], "id");
                    if !ids.is_empty() {
                        session.append("recall", verb, ids, Vec::new());
                    }
                }
                "remember" => {
                    let ids = body
                        .get("created")
                        .and_then(Value::as_array)
                        .map(|created| {
                            created
                                .iter()
                                .filter_map(|c| c.as_str().or_else(|| c.get("id")?.as_str()))
                                .map(str::to_owned)
                                .collect()
                        })
                        .unwrap_or_default();
                    session.append("write", verb, ids, Vec::new());
                }
                "reflect" => {
                    let ids = body
                        .get("id")
                        .and_then(Value::as_str)
                        .map(|id| vec![id.to_owned()])
                        .unwrap_or_default();
                    let cites = input
                        .get("derived_from")
                        .and_then(Value::as_array)
                        .map(|list| {
                            list.iter()
                                .filter_map(Value::as_str)
                                .map(str::to_owned)
                                .collect()
                        })
                        .unwrap_or_default();
                    session.append("write", verb, ids, cites);
                }
                _ => {}
            }
            None
        }
    }
}

fn stop(session: &Session, payload: &Value) -> Option<String> {
    let already = payload
        .get("stop_hook_active")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if already || session.wrote() || session.recalled().is_empty() {
        return None;
    }
    session.first_time("stop").then(|| STOP_NUDGE.to_owned())
}

// --- payload shapes -------------------------------------------------------

/// `(server, tool)` out of an MCP tool name, plugin-scoped or not.
///
/// `mcp__agmem__recall` and `mcp__plugin_agmem_agmem__recall` both name the
/// same tool; a plugin-bundled server gets the longer prefix.
fn mcp_parts(tool: &str) -> Option<(&str, &str)> {
    let rest = tool.strip_prefix("mcp__")?;
    rest.rsplit_once("__")
}

/// The tool's structured answer, whatever wrapping the payload gave it.
///
/// Claude Code hands PostToolUse the result as the tool emitted it, which for
/// an MCP tool may be the JSON object itself, the text rendering of it, or
/// the content-block array carrying that text. All three are tried.
fn response_body(response: &Value) -> Value {
    match response {
        Value::Object(_) => response
            .get("content")
            .and_then(parse_blocks)
            .unwrap_or_else(|| response.clone()),
        Value::String(text) => serde_json::from_str(text).unwrap_or(Value::Null),
        Value::Array(_) => parse_blocks(response).unwrap_or(Value::Null),
        _ => Value::Null,
    }
}

fn parse_blocks(content: &Value) -> Option<Value> {
    content.as_array()?.iter().find_map(|block| {
        let text = block.get("text")?.as_str()?;
        serde_json::from_str(text).ok()
    })
}

/// The string field `key` of every object in the array at `path`.
fn strings_at(body: &Value, path: &[&str], key: &str) -> Vec<String> {
    let mut node = body;
    for step in path {
        node = match node.get(step) {
            Some(next) => next,
            None => return Vec::new(),
        };
    }
    node.as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get(key)?.as_str())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// A real `git push`, not the word inside a commit message or an echo.
fn is_push(command: &str) -> bool {
    let stripped = strip_quoted(command);
    stripped.split(['|', ';', '&', '\n']).any(|segment| {
        let mut words = segment.split_whitespace().skip_while(|w| *w == "(");
        let is_git = words
            .next()
            .is_some_and(|w| w == "git" || w.ends_with("/git"));
        if !is_git {
            return false;
        }
        // The subcommand is the first word that is not a global option;
        // `-C <dir>` and `-c <k=v>` take a value, the rest are flags.
        let mut words = words.peekable();
        while let Some(word) = words.peek() {
            if *word == "-C" || *word == "-c" {
                words.next();
                words.next();
            } else if word.starts_with('-') {
                words.next();
            } else {
                break;
            }
        }
        words.next() == Some("push")
    })
}

fn strip_quoted(command: &str) -> String {
    let mut out = String::with_capacity(command.len());
    let mut quote: Option<char> = None;
    for c in command.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '"' || c == '\'' => quote = Some(c),
            None => out.push(c),
        }
    }
    out
}

// --- git ------------------------------------------------------------------

/// The current branch, or `None` when detached, unborn, or outside a repo.
fn branch_of(cwd: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["branch", "--show-current"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let branch = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!branch.is_empty() && branch != "HEAD").then_some(branch)
}

/// A branch name as the tag carries it — the rule context-flow's hooks and
/// `/checkpoint` share, so the two sides of the tag cannot drift.
fn slug(branch: &str) -> String {
    let mut out = String::with_capacity(branch.len());
    let mut dash = false;
    for c in branch.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
            out.push(c);
            dash = false;
        } else if !dash {
            out.push('-');
            dash = true;
        }
    }
    let trimmed = out.trim_matches('-');
    let mut short: String = trimmed.chars().take(80).collect();
    if short.is_empty() {
        short = "detached".to_owned();
    }
    short
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_push_is_recognised_and_a_mention_is_not() {
        assert!(is_push("git push"));
        assert!(is_push("git push -u origin main"));
        assert!(is_push("cargo test && git push"));
        assert!(is_push("/usr/bin/git push"));
        assert!(is_push("git -C /repo push --force-with-lease"));
        assert!(!is_push("git commit -m 'push it'"));
        assert!(!is_push("echo \"git push\""));
        assert!(!is_push("grep push src/"));
        assert!(!is_push("git log --grep push"));
    }

    #[test]
    fn plugin_scoped_and_bare_tool_names_split_alike() {
        assert_eq!(mcp_parts("mcp__agmem__recall"), Some(("agmem", "recall")));
        assert_eq!(
            mcp_parts("mcp__plugin_agmem_agmem__reflect"),
            Some(("plugin_agmem_agmem", "reflect"))
        );
        assert_eq!(mcp_parts("Bash"), None);
    }

    #[test]
    fn a_response_is_read_through_any_of_its_wrappings() {
        let object = json!({"hits": [{"id": "A"}, {"id": "B"}]});
        let text = Value::String(object.to_string());
        let blocks = json!([{"type": "text", "text": object.to_string()}]);
        let wrapped = json!({"content": [{"type": "text", "text": object.to_string()}]});
        for shape in [&object, &text, &blocks, &wrapped] {
            assert_eq!(
                strings_at(&response_body(shape), &["hits"], "id"),
                vec!["A", "B"],
                "{shape}"
            );
        }
    }

    #[test]
    fn slugs_match_the_hook_scripts_rule() {
        assert_eq!(slug("fix/takeover-lock-order"), "fix-takeover-lock-order");
        assert_eq!(slug("release/v1.2.3+build"), "release-v1.2.3-build");
        assert_eq!(slug("///"), "detached");
        assert_eq!(slug("a b  c"), "a-b-c");
    }

    #[test]
    fn a_session_id_cannot_name_a_path() {
        assert_eq!(safe_name("../../etc/passwd"), "..-..-etc-passwd");
        assert!(!safe_name("a/b\\c").contains(['/', '\\']));
        assert_eq!(safe_name(".."), "nosession");
        assert_eq!(safe_name(""), "nosession");
    }

    #[test]
    fn the_log_round_trips_and_nudges_once() {
        let dir = tempfile::tempdir().expect("tempdir");
        let session = Session::from_payload(dir.path(), &json!({"session_id": "s1"}));
        assert!(session.recalled().is_empty());
        session.append("recall", "recall", vec!["A".into(), "B".into()], vec![]);
        session.append("recall", "recall", vec!["B".into(), "C".into()], vec![]);
        assert_eq!(session.recalled(), vec!["B", "C", "A"]);
        assert!(!session.wrote());
        assert!(session.first_time("push"));
        assert!(!session.first_time("push"));
        session.append("write", "reflect", vec!["D".into()], vec!["A".into()]);
        assert!(session.wrote());
    }

    #[test]
    fn stop_nudges_only_a_session_that_recalled_and_never_wrote() {
        let dir = tempfile::tempdir().expect("tempdir");
        let payload = json!({"session_id": "s2", "stop_hook_active": false});
        let session = Session::from_payload(dir.path(), &payload);
        assert_eq!(stop(&session, &payload), None, "nothing recalled");
        session.append("recall", "recall", vec!["A".into()], vec![]);
        assert!(stop(&session, &payload).is_some(), "recalled, not written");
        assert_eq!(stop(&session, &payload), None, "once per session");

        let fresh = Session::from_payload(dir.path(), &json!({"session_id": "s3"}));
        fresh.append("recall", "recall", vec!["A".into()], vec![]);
        fresh.append("write", "remember", vec!["B".into()], vec![]);
        assert_eq!(stop(&fresh, &json!({"session_id": "s3"})), None, "wrote");

        let looping = json!({"session_id": "s2", "stop_hook_active": true});
        assert_eq!(stop(&session, &looping), None, "already continuing");
    }

    #[test]
    fn post_tool_use_logs_recalls_and_writes_and_nudges_seams() {
        let dir = tempfile::tempdir().expect("tempdir");
        let base = json!({"session_id": "s4"});
        let session = Session::from_payload(dir.path(), &base);

        let recall = json!({
            "session_id": "s4",
            "tool_name": "mcp__plugin_agmem_agmem__recall",
            "tool_input": {"query": "q"},
            "tool_response": {"hits": [{"id": "A"}, {"id": "B"}]}
        });
        assert_eq!(post_tool_use(&session, &recall), None);
        assert_eq!(session.recalled(), vec!["A", "B"]);

        let reflect = json!({
            "session_id": "s4",
            "tool_name": "mcp__agmem__reflect",
            "tool_input": {"insight": "x", "derived_from": ["A"]},
            "tool_response": {"id": "C", "created": true}
        });
        assert_eq!(post_tool_use(&session, &reflect), None);
        let writes: Vec<Entry> = session
            .entries()
            .into_iter()
            .filter(|e| e.kind == "write")
            .collect();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].ids, vec!["C"]);
        assert_eq!(writes[0].cites, vec!["A"]);

        let other_server = json!({
            "session_id": "s4",
            "tool_name": "mcp__memory__recall",
            "tool_response": {"hits": [{"id": "Z"}]}
        });
        assert_eq!(post_tool_use(&session, &other_server), None);
        assert!(!session.recalled().contains(&"Z".to_owned()));

        let push = json!({
            "session_id": "s4",
            "tool_name": "Bash",
            "tool_input": {"command": "git push origin HEAD"},
            "tool_response": {"exit_code": 0}
        });
        assert!(post_tool_use(&session, &push).is_some());
        assert_eq!(post_tool_use(&session, &push), None, "once per session");

        let failed_push = json!({
            "session_id": "s5",
            "tool_name": "Bash",
            "tool_input": {"command": "git push"},
            "tool_response": {"exit_code": 1}
        });
        let s5 = Session::from_payload(dir.path(), &failed_push);
        assert_eq!(post_tool_use(&s5, &failed_push), None);

        let decision = json!({"session_id": "s4", "tool_name": "AskUserQuestion"});
        assert!(post_tool_use(&session, &decision).is_some());
    }
}
