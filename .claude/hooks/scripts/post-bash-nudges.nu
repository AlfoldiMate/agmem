#!/usr/bin/env nu
# PostToolUse(Bash): the nudges that need to see the command that just ran.
#
# 1. A successful `git push` is a checkpoint seam — work shipped, reasons still
#    in context. A hook cannot run /checkpoint itself; the nudge is the job.
# 2. A Bash call that reached for python or sed where nu is the house tool.
#    CLAUDE.md says this already, in a file loaded every session — and this
#    repo's own transcripts show it losing to habit on 18% of Bash calls. That
#    is the gap a hook closes: it fires only when the anti-pattern actually
#    appears, which is the conditional-rule case hooks exist for, rather than
#    spending prompt on a rule that is usually irrelevant.
#
# Two rules govern the text, both learned from the hook reference:
#
# - Factual, never imperative. Text framed as an out-of-band instruction trips
#   Claude's prompt-injection defences, and gets surfaced to the user as
#   suspicious rather than read as context.
# - Once per session per nudge. Repetition is what gets a hook switched off, and
#   the second copy of a landed point is pure noise. This one self-extinguishes:
#   the better the habit gets, the less it fires.
#
# Nothing here can block a tool call. PostToolUse runs after the command has
# already succeeded, and this returns additionalContext on exit 0 — no decision,
# no exit 2, so a bug in this file costs a missing nudge and nothing else.
const COMMON = path self "_common.nu"
use $COMMON *

const PUSH_NUDGE = ("git push succeeded — that is a checkpoint seam. Update the ledger and "
    + "branch state now, as /checkpoint would: decisions with their reasons, corrected "
    + "assumptions, Done/Next. Then continue, or suggest /clear if the task is finished.")

# Anti-pattern -> the house alternative. Anchored to the start of a command or
# to a separator, so a quoted mention (`grep 'sed -i' f`) does not trip it.
const IDIOMS = [
    [id, re, note];

    ["json-via-python"
     '(^|[|;&] *)python3? +-c[^|]*json'
     ("This project reads structured output through the nu MCP server. "
      + "`mcp__nu__evaluate` parses with `| from json` and keeps the whole result in "
      + "`$history`, so a follow-up question about the same data costs no re-run, "
      + "which a `python3 -c` one-liner cannot do.")]

    ["edit-via-shell"
     '(^|[|;&] *)(sed +-i|python3? +- +<<)'
     ("CLAUDE.md's Shell work section specifies nu for shell-driven edits: "
      + "`open --raw f | str replace <old> <new> | save -f f`. `str replace` is literal "
      + "unless given -r, where sed is always regex and this project's sources carry "
      + "$ { [ ? | on nearly every line. Both sed and python's str.replace exit 0 "
      + "having changed nothing when the pattern misses.")]
]

# Match a real push, not the word "push" inside a commit message or echo:
# strip quoted strings first, then look for a git invocation whose arguments
# include `push` before any command separator.
def is-push [cmd: string]: nothing -> bool {
    ($cmd
        | str replace -ra `"[^"]*"|'[^']*'` ""
        | str replace -ra '\s+' " ") =~ '(^|[;&|(]\s*)git\b[^;&|]*\bpush\b'
}

# True the first time this session asks, false every time after. The marker is a
# file in the temp dir rather than anything in the repo: it should die with the
# machine's next reboot, not follow the project into git.
def first-time? [session: string, id: string]: nothing -> bool {
    let marker = $nu.temp-dir | path join $"ctx-flow-nudge-($session)-($id)"
    if ($marker | path type) == "file" { return false }
    try { "" | save -f $marker }
    true
}

def main []: any -> nothing {
    let p = $in | payload
    try {
        let cmd = $p.tool_input?.command? | default ""
        if ($cmd | is-empty) { return }
        let ok = ($p.tool_response?.exit_code? | default 0) == 0
        let session = $p.session_id? | default "nosession"

        let push = if $ok and (is-push $cmd) { [$PUSH_NUDGE] } else { [] }
        let idioms = $IDIOMS
            | where {|i| $cmd =~ $i.re }
            | where {|i| first-time? $session $i.id }
            | get note

        let notes = $push ++ $idioms
        if ($notes | is-not-empty) { context "PostToolUse" ($notes | str join "\n\n") }
    }
}
