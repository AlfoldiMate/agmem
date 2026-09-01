#!/usr/bin/env nu

# Does the store hold what the outcome-proxy counters would count? (issue #86)
#
# #86's positive arm rewards a memory that gets cited in `derived_from` after
# being recalled; its negative arm penalises one superseded soon after. Both
# are derivable at read time from rows the schema already keeps — a citation
# lives on the citing row, a supersession is dated — so before any of it is
# built, this asks the only question that decides anything: *are there any
# observations to rank on?* Run it against the live dogfood store; the
# counters stay parked until the citation count clears the trigger recorded
# in `docs/eval/outcome-counters.md`.
#
#     nu scripts/outcome-probe.nu
#     nu scripts/outcome-probe.nu --store ~/somewhere/agmem.db
#
# Costs nothing but time: no model, no server — a snapshot copy and a few
# counts through `surreal sql`. Never points surreal at the original dir; the
# daemon holds its LOCK, and this must stay a read of a copy.

def main [
    --store: string = ""       # an agmem data dir; defaults to the live one
    --out: string = "docs/eval" # where the counts land as JSON
] {
    if (which surreal | is-empty) {
        error make {msg: "no `surreal` on PATH — install surrealdb to run the probe"}
    }
    let store = if ($store | is-empty) { default-store } else { $store | path expand }
    if not ($store | path exists) {
        error make {msg: $"no store at ($store)"}
    }

    # Snapshot first: the daemon holds the LOCK in the live dir, and surreal
    # refuses (or worse, contends) when pointed at it.
    let copy = ((mktemp -d) | path join "agmem.db")
    cp -r $store $copy
    rm -f ($copy | path join "LOCK")

    let counts = [
        [name, query];
        ["schema_version" "SELECT VALUE schema_version FROM meta:main;"]
        ["memories" "SELECT count() FROM memory GROUP ALL;"]
        ["live" "SELECT count() FROM memory WHERE invalid_at IS NONE GROUP ALL;"]
        ["superseded" "SELECT count() FROM memory WHERE invalid_reason = 'superseded' GROUP ALL;"]
        ["live_with_citations" "SELECT count() FROM memory WHERE invalid_at IS NONE AND array::len(derived_from ?? []) > 0 GROUP ALL;"]
        ["cited_rows" "SELECT count() FROM memory WHERE array::len(derived_from ?? []) > 0 GROUP ALL;"]
        ["with_writer" "SELECT count() FROM memory WHERE writer IS NOT NONE GROUP ALL;"]
        ["with_novelty" "SELECT count() FROM memory WHERE novelty IS NOT NONE GROUP ALL;"]
    ] | each {|row|
        {name: $row.name, value: (ask $copy $row.query)}
    }

    let measured = {
        probed_at: (date now | format date "%+")
        store: $store
        counts: ($counts | transpose --header-row --as-record)
    }
    mkdir $out
    $measured | to json | save -f ($out | path join "outcome-counts.json")
    $counts | print
    print -e $"wrote ($out)/outcome-counts.json"
}

# One statement against the snapshot, answered as a single number.
def ask [copy: string, query: string] {
    let spoken = (
        $query
        | surreal sql --endpoint $"surrealkv://($copy)" --ns agmem --db main --json --hide-welcome
        | complete
    )
    if $spoken.exit_code != 0 {
        error make {msg: $"surreal exited ($spoken.exit_code): ($spoken.stderr)"}
    }
    # `--json` prints one line: an array with one entry per statement. One
    # statement goes in, so its result is entry 0 — a count comes back as
    # [{count: n}], an empty result as [], a `SELECT VALUE` as bare scalars.
    let rows = (
        $spoken.stdout
        | lines
        | where ($it | str trim | is-not-empty)
        | last
        | from json
        | get 0?
        | default []
    )
    let first = ($rows | get 0? | default 0)
    if (($first | describe -d | get type) == "record") {
        $first | get count? | default 0
    } else {
        $first
    }
}

# Where the embedded store lives when nothing says otherwise.
def default-store [] {
    let base = if $nu.os-info.name == "macos" {
        [$nu.home-dir "Library" "Application Support" "dev.agmem.agmem"] | path join
    } else {
        [$nu.home-dir ".local" "share" "agmem"] | path join
    }
    $base | path join "agmem.db"
}
