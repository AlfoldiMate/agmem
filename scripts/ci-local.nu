#!/usr/bin/env nu

# What CI would have said, run here.
#
# The workflow's four steps, in order, with the one environment variable that
# makes them CI rather than a local run: `FASTEMBED_CACHE_DIR=/nonexistent`,
# the guard from issue #2 that turns an accidental model download into a loud
# failure. A test that loads the real model passes here and fails there, which
# is exactly how the suite stayed red from #11 to #25 without anyone noticing.
#
#     nu scripts/ci-local.nu
#
# Exits non-zero on the first failing step and prints that step's output. The
# steps are checked against `.github/workflows/ci.yml` on every run, so this
# script cannot quietly drift away from the thing it stands in for.

# CI's own commands, verbatim. Keep in step with the workflow — `drift` below
# says so out loud when they diverge.
const STEPS = [
    "cargo fmt --check"
    "cargo clippy --workspace --all-targets -- -D warnings"
    "cargo test --workspace"
]

# Short names for the report, in the same order.
const LABELS = ["fmt" "clippy" "test"]

# The workflow this stands in for.
const WORKFLOW = ".github/workflows/ci.yml"

# What CI sets that a shell here does not.
const CI_ENV = {FASTEMBED_CACHE_DIR: "/nonexistent"}

def main [
    --keep-going # run every step instead of stopping at the first failure
] {
    drift

    mut results = []
    # A list rather than a nullable: nu types a `mut` from its first value, so
    # one that starts as `null` cannot later hold a record.
    mut failures = []
    for step in ($STEPS | enumerate) {
        let label = ($LABELS | get $step.index)
        print -e $"($label): ($step.item)…"
        let started = (date now)
        let done = (run-step $step.item)
        let elapsed = ((date now) - $started)

        $results = ($results | append {
            step: $label
            result: (if $done.exit_code == 0 { "PASS" } else { "FAIL" })
            took: $elapsed
            note: (note $label $done)
        })
        if $done.exit_code != 0 {
            $failures = ($failures | append {label: $label, step: $step.item, done: $done})
            if not $keep_going { break }
        }
    }

    $results | print

    if ($failures | is-not-empty) {
        for failure in $failures {
            print -e $"\n($failure.label) failed: ($failure.step)\n"
            # stderr first: cargo puts its diagnostics there, and a failure
            # whose reason is buried under a passing step's stdout is a report
            # nobody reads.
            print -e ($failure.done.stderr | str trim)
            print -e ($failure.done.stdout | str trim)
        }
        exit 1
    }

    let where = $"rustc (^rustc --version | str replace 'rustc ' '' | str trim)"
    print -e $"all four steps green \(($where), FASTEMBED_CACHE_DIR=/nonexistent)"
}

# Run one command string under CI's environment.
def run-step [step: string] {
    let parts = ($step | split row " " | where {|word| $word | is-not-empty})
    with-env $CI_ENV {
        ^($parts | first) ...($parts | skip 1) | complete
    }
}

# The line worth carrying next to PASS: how many tests, how many skipped.
#
# `cargo test` prints one `test result:` line per suite, so the totals are a
# sum rather than a lookup — and the ignored count is the half that matters
# here, because the fix for a test that cannot run in CI is usually to ignore
# it, and an ignored test that nobody runs is a promise nobody keeps.
def note [label: string, done: record] {
    if $label != "test" { return "" }
    let lines = ($done.stdout | lines | where ($it | str starts-with "test result:"))
    if ($lines | is-empty) { return "" }
    let counted = {
        passed: (count $lines "passed")
        failed: (count $lines "failed")
        ignored: (count $lines "ignored")
    }
    $"($counted.passed) passed, ($counted.failed) failed, ($counted.ignored) ignored"
}

# Sum one word's number across every `test result:` line.
def count [lines: list<string>, word: string] {
    $lines
    | each {|line|
        $line
        # Concatenated rather than interpolated: `(` opens a subexpression
        # inside an interpolated string, so a regex group evaluated as one.
        | parse --regex ('(?<n>\d+) ' + $word)
        | get -o n.0
        | default "0"
        | into int
    }
    | math sum
}

# Say so when the workflow and this script no longer run the same thing.
#
# A stand-in that has drifted is worse than none: it reports green for a set of
# checks nobody is running upstream.
def drift [] {
    if not ($WORKFLOW | path exists) {
        print -e $"note: ($WORKFLOW) is missing — nothing to check these steps against"
        return
    }
    let upstream = (
        open --raw $WORKFLOW
        | lines
        | parse --regex '^\s*- run: (?<cmd>cargo .*)$'
        | get cmd
        | each {|cmd| $cmd | str trim}
    )
    let missing = ($upstream | where {|cmd| $cmd not-in $STEPS})
    let extra = ($STEPS | where {|cmd| $cmd not-in $upstream})
    if ($missing | is-not-empty) {
        print -e $"note: in ($WORKFLOW) but not run here: ($missing | str join '; ')"
    }
    if ($extra | is-not-empty) {
        print -e $"note: run here but not in ($WORKFLOW): ($extra | str join '; ')"
    }
}
