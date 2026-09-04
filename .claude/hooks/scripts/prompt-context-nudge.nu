#!/usr/bin/env nu
# UserPromptSubmit: the session-length nudge. CLAUDE.md says to prefer several
# short sessions chained through memory over one long one, and nothing
# enforces it — the 2026-09-03 token audit found the two largest sessions ran
# 400+ turns at ~267k tokens of context each and never cleared, a fifth of all
# cache-read between them. Cache-read is 91% main-thread and scales with
# context × turns, so the cheapest token is the one a fresh session never
# carries. This hook says so, once, when the context is large enough for it
# to matter, and again only when it has grown by a further step.
#
# The number is exact, not a proxy: the transcript's last assistant line
# carries the API's `usage` for that turn, and input + cache_read +
# cache_creation is the context that turn was served with. Reading it costs a
# `tail -c` of the file's last 128 kB, no full parse — a session's transcript
# runs to tens of MB. When no usage line is in reach the file size stands in,
# at the bytes-per-token ratio measured across this repo's own transcripts.
#
# Once-gating lives in a temp-dir marker holding the last level nudged at,
# keyed by session id AND transcript path: a resumed session keeps both and
# stays quiet until the next step; a /clear gets a new transcript and starts
# over. Same safety rule as every hook here: wrapped in try, exit 0 — a bug
# costs a missing nudge, never a broken prompt.
const COMMON = path self "_common.nu"
use $COMMON *

# First nudge at this many context tokens; then every STEP more.
const THRESHOLD = 120000
const STEP = 40000

# Fallback only: transcript bytes per context token (median across the
# repo's 69 transcripts at end of file: median 6.5, measured 2026-09-04; the
# exact path found a usage line in every one of them, so this rarely runs).
const BYTES_PER_TOKEN = 6

# Bytes read from the end of the transcript to find the last usage line.
const TAIL = 131072

# Context tokens the last assistant turn was served with, or null.
def context-tokens [transcript: string]: nothing -> any {
    let chunk = try { ^tail -c $TAIL $transcript | complete | get stdout } catch { return null }
    let hits = $chunk | lines | where {|l| ($l | str contains '"usage"') and ($l | str contains '"cache_read_input_tokens"') }
    if ($hits | is-empty) { return null }
    let line = $hits | last
    let u = try { $line | from json | get message.usage } catch { return null }
    ([input_tokens cache_read_input_tokens cache_creation_input_tokens]
        | each {|k| $u | get $k -o | default 0 } | math sum)
}

# The marker's recorded level for this session+transcript, or 0.
def last-level [marker: string, transcript: string]: nothing -> int {
    let m = try { open --raw $marker | from json } catch { return 0 }
    if ($m.transcript? | default "") == $transcript { $m.level? | default 0 } else { 0 }
}

def main []: any -> nothing {
    let p = $in | payload
    try {
        let transcript = $p.transcript_path? | default ""
        if ($transcript | is-empty) or (($transcript | path type) != "file") { return }
        let threshold = env-int CTX_FLOW_CONTEXT_NUDGE_TOKENS $THRESHOLD
        let step = env-int CTX_FLOW_CONTEXT_NUDGE_STEP $STEP

        let tokens = context-tokens $transcript
            | default {|| (ls -l $transcript | get 0.size | into int) / $BYTES_PER_TOKEN | math round }
        if $tokens < $threshold { return }

        # The step level this context sits at: 120k → 120000, 175k → 160000.
        let level = $threshold + ((($tokens - $threshold) / $step | math floor) * $step)
        let session = $p.session_id? | default "nosession"
        let marker = $nu.temp-dir | path join $"ctx-flow-context-($session)"
        if $level <= (last-level $marker $transcript) { return }
        try { { transcript: $transcript, level: $level } | to json -r | save -f $marker }

        let k = ($tokens / 1000 | math round)
        context "UserPromptSubmit" ($"Context ~($k)k tokens. /checkpoint then /clear: a fresh session "
            + "starts from the briefing at a fraction of the cost.")
    }
}
