#!/usr/bin/env nu
# SessionStart: reload durable memory. This is the half of the loop that makes
# /clear cheap — clear the window, and the ledger walks back in by itself.
#
# When the session (re)starts because of a compaction, the payload says so
# (`source: "compact"`), so the truncation warning needs no marker file.
const COMMON = path self "_common.nu"
use $COMMON *

const DEFAULT_MAX_LINES = 400

const HEADER = ("Durable project memory, reloaded from disk. Treat it as established fact "
    + "and do not re-derive what it records; verify a specific entry only before "
    + "acting on it. Keep it current with /checkpoint.")

const COMPACTED = ("NOTE: context was compacted immediately before this. Anything not in the "
    + "memory below or on disk is gone — re-read files before assuming their "
    + "contents, and do not trust remembered line numbers.")

# One memory file rendered for injection, or null when it is absent or empty.
def block [file: any, label: string, root: string, max: int]: nothing -> any {
    if $file == null or ($file | path type) != "file" { return null }

    let lines = open --raw $file | lines
    let body = $lines | take $max | str join "\n" | str trim
    if ($body | is-empty) { return null }

    let rendered = $"--- ($label) \(($file | path relative-to $root)) ---\n($body)"
    if ($lines | length) <= $max { return $rendered }
    $"($rendered)\n[truncated at ($max) lines — read the file for the rest]"
}

def reload [p: record] {
    let max = env-int "CTX_FLOW_LEDGER_MAX_LINES" $DEFAULT_MAX_LINES

    let r = paths $p
    let blocks = [
        (block $r.ledger "REPO LEDGER — durable, shared across all branches" $r.root $max)
        (block $r.state $"BRANCH STATE — ($r.branch)" $r.root ($max // 2))
    ] | compact

    let memory = if ($blocks | is-empty) { [] } else {
        [$"($HEADER)\n\n($blocks | str join "\n\n")"]
    }

    let warn = if ($p.source? | default "") == "compact" { [$COMPACTED] } else { [] }
    let mismatch = layout-check (cwd-of $p)
    let parts = $warn
        | append (if $mismatch == null { [] } else { [$mismatch] })
        | append $memory
    if ($parts | is-not-empty) { context "SessionStart" ($parts | str join "\n\n") }
}

def main []: any -> nothing {
    let p = $in | payload
    try { reload $p }
}
