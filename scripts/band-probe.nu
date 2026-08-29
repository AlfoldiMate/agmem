#!/usr/bin/env nu

# Where does a real contradiction actually land? (design §5.5, issue #25)
#
# `consolidate` splits pairs by cosine, and the thresholds were chosen from the
# write gate rather than measured: `[0.75, 0.90)` was "same subject, different
# statement" and `≥ 0.90` was "the same claim twice". This asks the embedder.
#
# Each pair is two claims a working agent could plausibly hold at once, the
# second contradicting the first — except `control`, which names the same
# subject and disagrees about nothing. `remember` reports its own nearest
# neighbour with the similarity attached, so writing A and then B is a direct
# read of `cosine(A, B)` through the same code path `consolidate` compares on.
#
#     nu scripts/band-probe.nu
#     nu scripts/band-probe.nu --binary target/release/agmem --out docs/eval/bands
#
# Costs nothing but time: no model in the loop but the local embedder, one
# throwaway store per pair.

# One claim and its contradiction. The last row is the control.
const PAIRS = [
    [name, a, b];
    ["formatter" "The user formats Python with black." "The user formats Python with ruff format, and black is uninstalled."]
    ["testrunner" "The atlas test suite is run with cargo nextest run." "Atlas tests are run with plain cargo test, because nextest is not installed on the build machines."]
    ["platforms" "The atlas release build targets ubuntu and macos." "Atlas releases are built for linux only; the macos runner was dropped in June."]
    ["window" "Atlas deploys go out on Fridays." "Atlas deploys never go out on a Friday; the release window is Monday to Wednesday."]
    ["staging" "The atlas staging database is a copy of production." "The atlas staging database is seeded with synthetic rows and shares nothing with production."]
    ["packages" "Atlas uses npm to manage its JavaScript packages." "Atlas moved to pnpm for JavaScript packages; npm is no longer used."]
    ["logging" "Atlas writes its log output to stdout." "Atlas writes its log output to stderr, because stdout carries the protocol."]
    ["control" "The atlas test suite is run with cargo nextest run." "Project atlas is deployed by running bin/ship.sh from the repository root."]
]

def main [
    --binary: string = "target/release/agmem" # the agmem to measure
    --out: string = "docs/eval/bands"         # where the table lands
    --model-cache: string = ""                # FASTEMBED_CACHE_DIR
] {
    let binary = ($binary | path expand)
    if not ($binary | path exists) {
        error make {msg: $"no agmem at ($binary) — cargo build --release first"}
    }
    let cache = if ($model_cache | is-empty) { default-model-cache } else { $model_cache }
    mkdir $out

    let measured = (
        $PAIRS
        | each {|pair|
            let data = (mktemp -d)
            call $binary $data $cache "remember" {memories: [(claim $pair.a)]}
            let second = (call $binary $data $cache "remember" {memories: [(claim $pair.b)]})
            # A pair at or over the write gate never becomes two live rows at
            # all: `remember` refuses the second one and hands back the first,
            # with its text. That is a *result*, not a failed measurement —
            # three of the seven contradictions land there.
            let blocked = ($second.duplicates | where id != null)
            let related = ($second.related | where id != null)
            let neighbour = ($blocked ++ $related)
            {
                pair: $pair.name
                similarity: (if ($neighbour | is-empty) { null } else { $neighbour | get 0.similarity })
                gate: (if ($blocked | is-not-empty) { "blocked" } else if ($related | is-not-empty) { "written, reported" } else { "written, silent" })
                a: $pair.a
                b: $pair.b
            }
        }
        | sort-by similarity --reverse
    )

    $measured | to json | save -f ($out | path join "pairs.json")
    $measured | select pair similarity gate | print
    print -e $"wrote ($out)/pairs.json"
}

# The claim as an agent would file it: one subject, so the contradiction arm
# has the entity it requires.
def claim [content: string] {
    {content: $content, kind: "fact", entities: ["atlas"]}
}

# One tool call, one process.
#
# rmcp answers concurrently, so a batch of requests written to one stdin does
# not happen in the order it was written — a second `remember` can be embedded
# before the first has landed, which reads as a pair that resembles nothing.
# One process per call is what buys an ordering from a pipe.
def call [binary: string, data: string, cache: string, name: string, args: record] {
    let wire = (
        [
            {
                jsonrpc: "2.0"
                id: 1
                method: "initialize"
                params: {
                    protocolVersion: "2025-06-18"
                    capabilities: {}
                    clientInfo: {name: "band-probe", version: "1"}
                }
            }
            {jsonrpc: "2.0", method: "notifications/initialized"}
            {jsonrpc: "2.0", id: 2, method: "tools/call", params: {name: $name, arguments: $args}}
        ]
        | each {|message| $message | to json --raw}
        | str join "\n"
    )
    let spoken = (
        $wire
        | with-env {AGMEM_DATA: $data, AGMEM_SPACE: "bands", FASTEMBED_CACHE_DIR: $cache} {
            ^$binary --no-daemon | complete
        }
    )
    if $spoken.exit_code != 0 {
        error make {msg: $"agmem exited ($spoken.exit_code): ($spoken.stderr)"}
    }
    let reply = (
        $spoken.stdout
        | lines
        | where ($it | str starts-with "{")
        | each {|line| $line | from json}
        | where ($it.id? | default 0) == 2
    )
    if ($reply | is-empty) { error make {msg: $"no reply: ($spoken.stdout)"} }
    # A refused call has `content` and no `structuredContent`, and its text
    # says why. Reading past it would report a measurement that never happened.
    if "structuredContent" not-in ($reply | get 0.result | columns) {
        error make {msg: $"($name) refused: ($reply | get 0.result | to json --raw)"}
    }
    $reply | get 0.result.structuredContent
}

# Where agmem keeps its ONNX model when nothing says otherwise.
def default-model-cache [] {
    let base = if $nu.os-info.name == "macos" {
        [$nu.home-dir "Library" "Application Support" "dev.agmem.agmem"] | path join
    } else {
        [$nu.home-dir ".local" "share" "agmem"] | path join
    }
    $base | path join "models"
}
