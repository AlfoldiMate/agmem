#!/usr/bin/env nu

# The whole #133 pipeline for one candidate (docs/eval/embed-models.md):
# dump vectors, eval recordings, latency, the pair-rank AUC, the threshold
# table and the scorecard at floor 0 and at the default floor. Logs land in
# docs/eval/embed-models/<id>/; the summary lines are printed at the end.
#
#     nu scripts/embed-candidates-run.nu <candidate-id> --dump /tmp/probe-dump.json
#     nu scripts/embed-candidates-run.nu --all --dump /tmp/probe-dump.json
#
# Needs the models fetched (scripts/embed-candidates-fetch.nu) and a store
# dump (docs/eval/pair-rank.md, Inputs). Release builds; the first run
# compiles.

const IDS = ["bge-small-en-v1.5-q" "embeddinggemma-300m-q" "arctic-embed-m-v2.0-int8" "qwen3-embedding-0.6b-int8"]

def main [
    id?: string          # one candidate id
    --all                # every candidate, control first
    --dump: string       # the store dump JSON
    --skip-latency       # leave the latency rows alone (they need a quiet machine)
] {
    if ($dump | is-empty) { error make {msg: "--dump is required"} }
    let ids = if $all { $IDS } else if ($id | is-empty) { error make {msg: "give an id or --all"} } else { [$id] }
    let root = (git rev-parse --show-toplevel | str trim)
    for id in $ids {
        let out = [$root "docs/eval/embed-models" $id] | path join
        mkdir $out
        print $"== ($id)"
        let vectors = [$root "target/eval" $"($id)-dump-vectors.json"] | path join
        let fixtures = [$root "crates/agmem-server/tests/fixtures/eval/candidates" $id] | path join

        step $out "embed-dump" {
            with-env {AGMEM_CANDIDATE: $id, AGMEM_DUMP: $dump} {
                cargo test -p agmem-embed --features candidates --release --test candidates -- --ignored --nocapture embed_dump
            }
        }
        step $out "regenerate" {
            with-env {AGMEM_CANDIDATE: $id} {
                cargo test -p agmem-embed --features candidates --release --test fastembed -- --ignored --nocapture regenerate_eval_vectors
            }
        }
        if not $skip_latency {
            step $out "latency" {
                with-env {AGMEM_CANDIDATE: $id} {
                    cargo test -p agmem-embed --features candidates --release --test candidates -- --ignored --nocapture latency
                }
            }
        }
        step $out "pair-rank" {
            cd $out
            uv run ([$root "scripts/pair-rank-probe.py"] | path join) $dump --vectors $vectors
        }
        step $out "thresholds" {
            uv run ([$root "scripts/embed-thresholds.py"] | path join) $dump $vectors --dims 512,256,128
        }
        for floor in ["0" "default"] {
            let knobs = if $floor == "default" { {AGMEM_EVAL_VECTORS_DIR: $fixtures} } else { {AGMEM_EVAL_VECTORS_DIR: $fixtures, AGMEM_ABSTENTION_FLOOR: $floor} }
            step $out $"scorecard-floor-($floor)" {
                with-env $knobs {
                    cargo test -p agmem-server --features eval-knobs --release --test eval -- --ignored --nocapture candidate_scorecard
                }
            }
        }
        print (summary $out)
    }
}

# Run one step, keep its whole output, print only its status.
def step [out: string, name: string, run: closure] {
    let log = [$out $"($name).log"] | path join
    let result = (do $run | complete)
    ($result.stdout + "\n" + $result.stderr) | save -f $log
    print $"   ($name): exit ($result.exit_code) → ($log | path basename)"
    if $result.exit_code != 0 {
        print ($result.stderr | lines | last 15 | str join "\n")
    }
}

# The lines the doc's Results table is filled from.
def summary [out: string] {
    let grab = {|file, pattern| (try { open --raw ([$out $file] | path join) } catch { "" }) | lines | where $it =~ $pattern }
    [
        (do $grab "embed-dump.log" 'rows, \d+d')
        (do $grab "latency.log" 'p50')
        (do $grab "pair-rank.log" 'cosine \(control\)')
        (do $grab "thresholds.log" '^(supersede floor|duplicate gate|corrected vs noise|==)')
        (do $grab "scorecard-floor-0.log" '^SUMMARY')
        (do $grab "scorecard-floor-default.log" '^SUMMARY')
    ] | flatten | str join "\n"
}
