#!/usr/bin/env nu
# Move the retired `.claude/notes/` dropbox into agmem as documents, once.
#
# Deterministic on purpose — the kind comes from the filename, the title is
# the stem, nothing is distilled — so it is a script and not a prompt. Each
# imported file is renamed `<name>.imported` in place (the ledger convention:
# a second run is a no-op, and the rename is reversible); deleting the
# directory stays the user's action, which /agmem-import asks for last.
#
# What it does not import, and says so: `.imported` files, anything that is
# not `.md`/`.txt`, subdirectories (`state/`, `agmem-archive/` — their claims
# went in through /agmem-import already), and a file over the store's cap.
#
# Usage, from anywhere inside the repo:
#     nu .claude/scripts/import-notes.nu --dry-run   # the table, no writes
#     nu .claude/scripts/import-notes.nu
const COMMON = path self "../hooks/scripts/_common.nu"
use $COMMON [paths]

const CAP = 100000
const TAG = "legacy-notes"

# The filename rule, first match wins. `research-` files are reports (#154's
# wording); audits and analyses are reviews; a `.txt` is an eval log — a probe.
def kind-of [name: string]: nothing -> record {
    let parsed = $name | path parse
    let stem = $parsed.stem
    if ($stem starts-with "plan-") or ($stem ends-with "-plan") {
        { kind: "plan", mime: null }
    } else if ($stem starts-with "review-") {
        { kind: "review", mime: null }
    } else if ($stem starts-with "research-") {
        { kind: "report", mime: null }
    } else if ($stem =~ 'audit|analysis') {
        { kind: "review", mime: null }
    } else if $parsed.extension == "txt" {
        { kind: "probe", mime: "text/plain" }
    } else {
        { kind: "other", mime: null }
    }
}

def main [--dry-run]: nothing -> nothing {
    let dir = (paths { cwd: $env.PWD }).notes
    if ($dir | path type) != "dir" {
        print $"no notes directory at ($dir) — nothing to import"
        return
    }

    let entries = ls $dir
    let files = $entries
        | where type == file
        | where { ($in.name | path parse | get extension) in ["md" "txt"] }
        | get name
    let done = $entries | where name ends-with ".imported" | get name
    let leftovers = $entries | where name not-in $files and name not-in $done | get name

    let rows = $files | each {|f|
        let content = open --raw $f
        let chars = $content | str length
        let k = kind-of $f
        let title = $f | path parse | get stem
        let base = { file: ($f | path basename), kind: $k.kind, chars: $chars }
        if $chars > $CAP {
            $base | merge { id: "", uri: $"skipped: over the ($CAP)-char cap" }
        } else if $dry_run {
            $base | merge { id: "", uri: "(dry run)" }
        } else {
            let mime_args = if $k.mime == null { [] } else { ["--mime" $k.mime] }
            let r = $content
                | ^agmem doc put --kind $k.kind --title $title --tag $TAG ...$mime_args
                | complete
            if $r.exit_code != 0 {
                $base | merge { id: "", uri: $"FAILED: ($r.stderr | str trim | lines | first | default '')" }
            } else {
                let parts = $r.stdout | str trim | split row " "
                mv $f $"($f).imported"
                $base | merge { id: ($parts | first), uri: ($parts | get 1 | default "") }
            }
        }
    }

    print ($rows | table -i false --width 160)
    if not ($done | is-empty) {
        print $"already imported \(.imported\): ($done | length)"
    }
    if not ($leftovers | is-empty) {
        print ""
        print "not documents — move or delete by hand:"
        $leftovers | each {|l| print $"  ($l)" } | ignore
    }
}
