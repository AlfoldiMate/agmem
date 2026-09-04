#!/usr/bin/env nu
# Store stdin as an agmem document and print its address — the one way a
# subagent hands back anything longer than its return contract.
#
# Why a wrapper and not `agmem doc put` inline: the document must carry the
# same `branch:<slug>` tag the SessionStart hook announces and /checkpoint
# writes, so `agmem doc list --tag branch:<slug>` is "this branch's
# documents". That slug has one rule, in `_common.nu`; an agent substituting
# it by hand drifts, and on a detached HEAD an empty expansion becomes
# `--tag ""`. The wrapper resolves the tag through `paths`, adds
# `agent:<name>` so /checkpoint knows which role's playbook a proposal joins,
# and refuses loudly where the raw CLI would fail quietly: a binary without
# `agmem doc`, an empty document, one over the store's size cap.
#
# Usage, content on stdin:
#     nu .claude/scripts/doc-put.nu <agent> <kind> <title> [--mime text/plain]
# Prints `<id> memory://<space>/doc/<id>` — return it as `DOC: <id> <uri>`.
const COMMON = path self "../hooks/scripts/_common.nu"
use $COMMON [paths]

# `MAX_EPISODE_CHARS` in the server — a document past it is refused, not cut.
const CAP = 100000

def main [
    agent: string   # the role writing: architect, runner, browser, scout, tracker, verifier
    kind: string    # plan | review | report | probe | transcript | other
    title: string   # `<kind>-<topic>[-<date>]`; a second put under one title is a new version
    --mime: string  # media type when not markdown, e.g. text/plain
]: nothing -> nothing {
    # A script's `main` does not receive stdin as pipeline input; read it.
    let content = try { ^cat | complete | get stdout } | default ""
    if ($content | str trim | is-empty) {
        print -e "doc-put: nothing on stdin — pipe the document in"
        exit 2
    }
    let chars = $content | str length
    if $chars > $CAP {
        print -e $"doc-put: ($chars) chars is over the ($CAP) cap — write it to /tmp/($title).md and return that path"
        exit 2
    }
    let probe = try { ^agmem doc --help | complete | get exit_code } | default 1
    if $probe != 0 {
        print -e "doc-put: agmem has no `doc` subcommand (needs 0.2.0+) — write to /tmp and return the path"
        exit 2
    }

    let p = paths { cwd: $env.PWD }
    let tags = [$"agent:($agent)"] | append (if $p.tag == null { [] } else { [$p.tag] })
    let tag_args = $tags | each {|t| ["--tag" $t] } | flatten
    let mime_args = if $mime == null { [] } else { ["--mime" $mime] }

    let r = $content | ^agmem doc put --kind $kind --title $title ...$tag_args ...$mime_args | complete
    if $r.exit_code != 0 {
        print -e ($r.stderr | str trim)
        exit $r.exit_code
    }
    print -n $r.stdout
}
