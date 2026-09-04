#!/usr/bin/env nu
# PreToolUse(Read | Bash): the flood guard. The 2026-09-03 token audit of this
# repo's 71 sessions found that whole-file reads were the single largest
# tool-result cost — a bare `Read` with neither `offset` nor `limit` was 28%
# of all result characters, and `cat` / `sed -n` / `head` / `tail` used as a
# Read another 18%. Every one of the fifteen largest results was one file
# read whole. CLAUDE.md already says to window; this hook makes the window
# the default, at the one moment the rule applies.
#
# What it denies, and only that:
#
# - `Read` with no `offset` and no `limit` of a file that is both longer than
#   MAX_LINES and larger than SMALL_BYTES. Any windowed call passes, including
#   one whose `limit` covers the whole file — that is the explicit way to say
#   "all of it", and it costs one round trip only when the guard fired first.
# - A Bash statement whose *output-producing* command (the last in its pipe)
#   is `cat`/`bat`/`nl`, `head`/`tail` with a large count, or `sed` printing a
#   large or unbounded range, on such a file. `cat big | wc -l` passes: the
#   flood never reaches the model. A windowed `sed -n '40,120p'` passes: it
#   is Read offset/limit spelled in shell, which the auto-mode preamble asks
#   for. Redirects and heredocs pass: they are writes.
#
# Everything the guard cannot resolve — a path through a variable, a file
# that does not exist yet, a command shape it does not parse — passes. The
# guard fails open, always: a hook that denied on doubt would cost more turns
# than the floods it prevents. Same safety rule as every hook here: wrapped
# in try, exit 0.
const COMMON = path self "_common.nu"
use $COMMON *

# Longer than this AND larger than SMALL_BYTES is a flood. Both knobs are
# environment variables (see env-int in _common.nu).
const MAX_LINES = 300
const SMALL_BYTES = 12000

# Files Read handles as something other than text — never guarded.
const BINARY_EXT = [png jpg jpeg gif webp bmp svg pdf ipynb]

# {lines, bytes} of a text file, or null when it is absent, a directory,
# unreadable, binary, or already small enough that counting is a waste.
def stats [path: string]: nothing -> any {
    if ($path | path type) != "file" { return null }
    let bytes = try { ls -l $path | get 0.size | into int } catch { return null }
    if $bytes <= (env-int CTX_FLOW_READ_SMALL_BYTES $SMALL_BYTES) { return null }
    let text = try { open --raw $path } catch { return null }
    if ($text | describe) != "string" { return null }
    { lines: ($text | lines | length), bytes: $bytes }
}

def flood? [s: any]: nothing -> bool {
    $s != null and $s.lines > (env-int CTX_FLOW_READ_MAX_LINES $MAX_LINES)
}

def kchars [n: int]: nothing -> string { $"(($n / 1000) | math round)k chars" }

def deny [reason: string] {
    print -n ({ hookSpecificOutput: {
        hookEventName: "PreToolUse"
        permissionDecision: "deny"
        permissionDecisionReason: $reason
    } } | to json -r)
}

# --- Read ----------------------------------------------------------------

def guard-read [p: record] {
    let t = $p.tool_input? | default {}
    if ($t.offset? != null) or ($t.limit? != null) { return }
    let path = $t.file_path? | default ""
    if ($path | is-empty) { return }
    if (($path | path parse | get extension | str lowercase) in $BINARY_EXT) { return }
    let s = stats $path
    if not (flood? $s) { return }
    deny ($"($path | path basename) is ($s.lines) lines \((kchars $s.bytes)\); a bare Read pulls all of it. "
        + "Read again with offset+limit \(~120 lines a window\), or let a scout/Explore return the part needed.")
}

# --- Bash ----------------------------------------------------------------

# Resolve a command-line token to a path, or null when it cannot be one.
def resolve [tok: string, cwd: string]: nothing -> any {
    if ($tok | is-empty) or ($tok starts-with "-") or ($tok starts-with "$") or ($tok == "-") { return null }
    let p = if ($tok starts-with "~") { $tok | path expand } else if ($tok starts-with "/") { $tok } else { $cwd | path join $tok }
    if ($p | path type) == "file" { $p } else { null }
}

# The value of `-n N` / `-nN` / `-N` / `--lines=N` in a head/tail arg list,
# or null when absent. A `+K` (tail) comes back as a negative number so the
# caller can tell "last N" from "from line K".
def count-flag [args: list<string>]: nothing -> any {
    mut i = 0
    while $i < ($args | length) {
        let a = $args | get $i
        let next = $args | get ($i + 1) -o | default ""
        let v = if $a in ["-n" "--lines"] { $next
            } else if ($a starts-with "--lines=") { $a | str substring 8..
            } else if ($a =~ '^-n.+') { $a | str substring 2..
            } else if ($a =~ '^-\d+$') { $a | str substring 1..
            } else { null }
        if $v != null {
            if ($v =~ '^\+\d+$') { return (0 - ($v | str substring 1.. | into int)) }
            if ($v =~ '^\d+$') { return ($v | into int) }
            return null
        }
        $i += 1
    }
    null
}

# Lines a `sed` invocation prints from a file of `total` lines: whole file
# without -n; with -n, the size of an `N,Mp` / `N,$p` / `Np` range; null for
# a script the guard does not read (`/re/p`, `s///p`), which passes.
def sed-lines [args: list<string>, total: int]: nothing -> any {
    if ("-i" in $args) or ($args | any {|a| $a starts-with "-i" }) { return null }   # an edit, not a read
    let quiet = $args | any {|a| $a =~ '^-[a-zA-Z]*n' }
    if not $quiet { return $total }
    let scripts = $args | where {|a| $a =~ '^[0-9$]' }
    if ($scripts | is-empty) { return null }
    let s = $scripts | first
    let m = $s | parse -r '^(?<a>\d+|\$)(?:,(?<b>\d+|\$))?p$'
    if ($m | is-empty) { return null }
    let a = if $m.0.a == '$' { $total } else { $m.0.a | into int }
    let b = if ($m.0.b | is-empty) { $a } else if $m.0.b == '$' { $total } else { $m.0.b | into int }
    ([(([$b $total] | math min) - $a + 1), 0] | math max)
}

# The reading command at the end of one pipeline, as {cmd, args}, or null.
def reader-of [segment: string]: nothing -> any {
    if ($segment =~ '(^|[^<])<<|>') { return null }             # heredoc or redirect: a write
    let toks = $segment
        | str replace -ra `"([^"]*)"|'([^']*)'` '$1$2'          # unquote; keeps the inner text
        | str trim | split row -r '\s+'
        | where {|t| not ($t =~ '^[A-Za-z_][A-Za-z0-9_]*=') }   # drop leading FOO=bar assignments
    if ($toks | is-empty) { return null }
    let cmd = $toks | first | path basename
    if not ($cmd in [cat bat nl head tail sed]) { return null }
    { cmd: $cmd, args: ($toks | skip 1) }
}

# Lines this reader would print, or null when unbounded-by-guard (passes).
def printed-lines [r: record, cwd: string]: nothing -> any {
    let files = $r.args | each {|a| resolve $a $cwd } | compact
    if ($files | is-empty) { return null }
    let s = $files | each {|f| stats $f } | compact
    if ($s | is-empty) { return null }
    let total = $s | get lines | math sum
    match $r.cmd {
        "head" => {
            let n = count-flag $r.args | default 10
            if $n < 0 { null } else { [$n $total] | math min }
        }
        "tail" => {
            let n = count-flag $r.args | default 10
            if $n < 0 { $total + $n + 1 } else { [$n $total] | math min }
        }
        "sed" => { sed-lines $r.args $total }
        _ => { $total }
    }
}

def guard-bash [p: record] {
    let cmd = $p.tool_input?.command? | default ""
    if ($cmd | is-empty) { return }
    let cwd = cwd-of $p
    let max = env-int CTX_FLOW_READ_MAX_LINES $MAX_LINES
    for stmt in ($cmd | split row -r '\s*(?:&&|\|\||;|\n)\s*') {
        let last = $stmt | split row "|" | last | str trim
        let r = reader-of $last
        if $r == null { continue }
        let n = printed-lines $r $cwd
        if $n == null or $n <= $max { continue }
        deny ($"`($last | str substring ..<40)` would print ($n) lines into the context. "
            + "Read with offset+limit for a window \(or `sed -n 'a,bp'`\), or let a scout/Explore return the part needed.")
        return
    }
}

def main []: any -> nothing {
    let p = $in | payload
    try {
        match ($p.tool_name? | default "") {
            "Read" => { guard-read $p }
            "Bash" => { guard-bash $p }
            _ => {}
        }
    }
}
