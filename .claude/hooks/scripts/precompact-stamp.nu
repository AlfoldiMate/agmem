#!/usr/bin/env nu
# PreCompact: record in the ledger that context was silently lost.
#
# The stamp is a single line, rewritten in place with a running count — not one
# line per event. Appending would grow a file that is injected at every session
# start, and it would bury the signal you actually want: not that this happened
# once, but that it keeps happening. Three occurrences means you are
# checkpointing too late, and a count says so where a wall of identical
# comments does not.
const COMMON = path self "_common.nu"
use $COMMON *

const STAMP = '(?m)^<!-- ctx-flow: auto-compaction fired (\d+)x.*-->\n?'

def stamp-line [n: int]: nothing -> string {
    ($"<!-- ctx-flow: auto-compaction fired ($n)x, most recently "
        + $"((date now | format date '%Y-%m-%d')) — context was truncated without "
        + "asking. Run /checkpoint earlier, or a single tool result is very "
        + "large. -->")
}

def stamp [p: record] {
    if ($p.trigger? | default "") != "auto" { return }   # a deliberate /compact needs no warning
    let ledger = (paths $p).ledger
    if ($ledger | path type) != "file" { return }

    let text = open --raw $ledger
    let prior = $text | parse -r $STAMP | get capture0? | default [] | first | default "0"
    let body = $text | str replace -ra $STAMP "" | str trim --right --char "\n"

    # Write through a temp file: a killed hook must never leave a half-written
    # ledger, which is the one file a fresh session trusts completely.
    let tmp = $"($ledger).ctx-flow-tmp"
    $"($body)\n\n(stamp-line (($prior | into int) + 1))\n" | save -f $tmp
    mv -f $tmp $ledger
}

def main []: any -> nothing {
    let p = $in | payload
    try { stamp $p }
}
