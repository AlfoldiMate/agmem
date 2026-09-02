#!/usr/bin/env nu
# PostToolUse(Bash): the nudge that needs to see the command that just ran —
# a Bash call that reached for python, sed or grep where the house tool is nu
# or ast-grep. CLAUDE.md says this already, in a file loaded every session —
# and this repo's own transcripts show it losing to habit on 18% of Bash
# calls. That is the gap a hook closes: it fires only when the anti-pattern
# actually appears, which is the conditional-rule case hooks exist for, rather
# than spending prompt on a rule that is usually irrelevant.
#
# The `git push` checkpoint nudge used to live here too. It is the agmem
# plugin's now (`agmem hook post-tool-use`), along with the other memory
# seams, so a project without this framework gets it as well.
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

    ["search-via-grep"
     '(^|[;&] *)(rg|grep)\b'
     ("This project routes structural questions — callers of a symbol, its "
      + "definition, anything shaped like a pattern — through `ast-grep` (a Bash "
      + "CLI), whose hit list does not match inside strings or comments the way "
      + "`rg`/`grep` do; plain-text search is what `rg`/`grep` stay right for. "
      + "Discovery like this is also the `scout` agent's job, so it need not land "
      + "in the main thread at all.")]
]

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
        let session = $p.session_id? | default "nosession"

        let notes = $IDIOMS
            | where {|i| $cmd =~ $i.re }
            | where {|i| first-time? $session $i.id }
            | get note
        if ($notes | is-not-empty) { context "PostToolUse" ($notes | str join "\n\n") }
    }
}
