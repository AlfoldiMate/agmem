#!/usr/bin/env nu

# Does an agent actually reach for agmem? (design §9 risk 4, issue #23)
#
# The tool descriptions are the product surface: nothing else decides whether a
# model calls `recall` before answering or `remember` after learning something.
# That cannot be unit-tested, so this drives real headless Claude Code sessions
# against a throwaway store and counts what each one reached for.
#
# Each scenario runs in its own data dir and its own empty working directory,
# with `--strict-mcp-config` and no settings sources, so agmem is the only MCP
# server present and nothing in the developer's own configuration leaks in. The
# prompts never mention memory — asking for a `recall` and getting one measures
# instruction-following, not the description.
#
#     nu scripts/desc-eval.nu --label before --runs 3
#     nu scripts/desc-eval.nu --label after --runs 3
#     nu scripts/desc-eval.nu report before after
#
# Results land in `docs/eval/<label>/`: one JSON per run plus a summary the
# report subcommand renders. Sessions cost money — 4 scenarios × `--runs` each.

# The four questions risk 4 actually asks, one scenario apiece.
#
# `want` passes when *any* of its tools was called (orientation is `context` or
# `recall`, either is the right instinct); `avoid` fails when any of its tools
# was called at all. A scenario with an empty `want` is testing restraint.
const SCENARIOS = [
    {
        name: "orient"
        asks: "does it read memory before answering something memory answers?"
        seed: [
            {
                content: "Project atlas is deployed by running bin/ship.sh from the repository root; the make deploy target was removed."
                kind: "fact"
                entities: ["atlas"]
                tags: ["deploy"]
            }
            {
                content: "The atlas test suite is run with cargo nextest run, not cargo test."
                kind: "fact"
                entities: ["atlas"]
            }
        ]
        prompt: "How do I deploy atlas?"
        want: ["recall" "context"]
        avoid: []
    }
    {
        name: "store"
        asks: "does it write a durable preference down without being told to?"
        seed: []
        prompt: "Heads up for future work on this codebase: I want library crates to use thiserror for their error types and binaries to use anyhow, never the other way round. It has bitten us twice."
        want: ["remember"]
        avoid: []
    }
    {
        name: "correct"
        asks: "does a contradicted claim get superseded rather than duplicated?"
        seed: [
            {
                content: "The user formats Python with black."
                kind: "fact"
                tags: ["identity"]
            }
        ]
        prompt: "I have moved off black — everything is formatted with ruff format now, and black is uninstalled. Note that for later."
        want: ["remember"]
        avoid: []
    }
    {
        name: "restraint"
        asks: "does it leave memory alone when there is nothing to remember?"
        seed: []
        prompt: "What is the capital of France? Answer in one word."
        want: []
        avoid: ["remember" "recall" "context" "forget" "inspect"]
    }
]

# Run every scenario `runs` times and write the results under `out/label`.
def main [
    --binary: string = "target/release/agmem" # the agmem to serve
    --model: string = "sonnet"                # the agent under test
    --runs: int = 3                           # sessions per scenario
    --label: string = "baseline"              # names this batch of results
    --out: string = "docs/eval"               # where results land
    --only: string = ""                       # comma-separated scenario names
    --isolated                                # turn the client's own auto-memory off,
                                              # so agmem is the only place to persist
    --desc: string = ""                       # JSON file of {tool: description},
                                              # served through AGMEM_TOOL_DESC_<TOOL>
    --model-cache: string = ""                # FASTEMBED_CACHE_DIR; shared so a
                                              # fresh data dir does not re-download
] {
    let binary = ($binary | path expand)
    if not ($binary | path exists) {
        error make {msg: $"no agmem at ($binary) — cargo build --release first"}
    }
    let cache = if ($model_cache | is-empty) { default-model-cache } else { $model_cache }
    let dir = ($out | path join $label)
    mkdir $dir

    let scenarios = if ($only | is-empty) {
        $SCENARIOS
    } else {
        let wanted = ($only | split row "," | each {|name| $name | str trim})
        $SCENARIOS | where name in $wanted
    }
    if ($scenarios | is-empty) {
        error make {msg: $"no scenario called ($only)"}
    }

    # `AGMEM_TOOL_DESC_<TOOL>` is how a batch runs wording the binary was not
    # built with — which is what makes an A/B possible without two binaries.
    let overrides = if ($desc | is-empty) {
        {}
    } else {
        open $desc
        | transpose tool text
        | reduce --fold {} {|row, acc|
            $acc | insert $"AGMEM_TOOL_DESC_($row.tool | str uppercase)" $row.text
        }
    }

    let results = (
        $scenarios
        | each {|scenario|
            1..$runs | each {|run|
                print -e $"($label) ($scenario.name) run ($run)/($runs)…"
                let result = (run-one $binary $model $cache $scenario $run $isolated $overrides)
                $result | to json | save -f ($dir | path join $"($scenario.name)-($run).json")
                $result
            }
        }
        | flatten
    )

    $results | to json | save -f ($dir | path join "runs.json")
    # What the agents in this batch actually read. Without it a saved result
    # is a number with no wording attached, which is the one thing a
    # before/after comparison cannot do without.
    descriptions $binary $cache $overrides
    | to json
    | save -f ($dir | path join "descriptions.json")
    $results | summarise | print
    print -e $"wrote ($dir)/runs.json"
}

# Render one or more finished batches side by side.
def "main report" [...labels: string, --out: string = "docs/eval"] {
    $labels
    | each {|label|
        open ($out | path join $label "runs.json")
        | summarise
        | insert label $label
    }
    | flatten
    | move label --before scenario
}

# Pass rate and what was called, per scenario.
def summarise [] {
    $in
    | group-by scenario
    | items {|scenario, runs|
        {
            scenario: $scenario
            runs: ($runs | length)
            passed: ($runs | where pass | length)
            # Blank rather than a number for a batch recorded before `served`
            # existed: a default of `true` here would report an unverified run
            # as a verified one, which is the failure this column exists to
            # catch.
            served: (
                if ($runs | all {|run| ($run | get -o served) == null}) {
                    null
                } else {
                    $runs | where ($it.served? | default false) | length
                }
            )
            calls: ($runs | get calls | math avg | math round --precision 1)
            tools: ($runs | get tools | flatten | uniq | sort | str join ",")
            superseded: ($runs | where superseded | length)
        }
    }
}

# One session: seed a store, ask one question, report what the agent reached for.
def run-one [
    binary: string
    model: string
    cache: string
    scenario: record
    run: int
    isolated: bool
    overrides: record
] {
    let data = (mktemp -d)
    let cwd = (mktemp -d)
    cd $cwd

    if ($scenario.seed | is-not-empty) {
        seed $binary $data $cache $scenario.seed $overrides
    }

    let config = ($cwd | path join "mcp.json")
    {
        mcpServers: {
            agmem: {
                command: $binary
                # No daemon: one store per scenario, gone with the temp dir.
                args: ["--no-daemon"]
                env: (agmem-env $data $cache $overrides)
            }
        }
    }
    | to json
    | save -f $config

    # `--settings` rather than a settings file: the client's own auto-memory
    # is the thing agmem is competing with, and a run that leaves it on is
    # measuring the competition rather than the description.
    let settings = if $isolated { ["--settings" '{"autoMemoryEnabled": false}'] } else { [] }

    let session = (
        ^claude -p $scenario.prompt
            --model $model
            --output-format stream-json --verbose
            --strict-mcp-config --mcp-config $config
            --permission-mode bypassPermissions
            --setting-sources ""
            --no-session-persistence
            --disable-slash-commands
            ...$settings
        | complete
    )
    let events = (
        $session.stdout
        | lines
        | where ($it | str starts-with "{")
        | each {|line| $line | from json}
    )
    let calls = (agmem-calls $events)
    let used = ($calls | get tool | uniq)
    let hit = (($scenario.want | is-empty) or ($scenario.want | any {|tool| $tool in $used}))
    let clean = ($scenario.avoid | all {|tool| $tool not-in $used})
    let result = ($events | where type == "result")

    {
        scenario: $scenario.name
        run: $run
        pass: ($hit and $clean)
        tools: $used
        calls: ($calls | length)
        # A correction is the expensive half of `remember`'s contract: storing
        # a contradiction instead is a pass on "did it write" and a failure on
        # everything the history chain exists for.
        superseded: (
            $calls
            | where tool == "remember"
            | any {|call|
                $call.input.memories?
                | default []
                | any {|memory| "supersedes" in ($memory | columns)}
            }
        )
        # Whether agmem was there at all. Without it, a server that failed to
        # start reads as an agent that chose not to call anything — which is a
        # pass on `restraint` and a failure everywhere else, both wrong.
        served: (
            $events
            | where type == "system"
            | get -o 0.mcp_servers
            | default []
            | any {|server| $server.name == "agmem" and $server.status == "connected"}
        )
        # Whether the client offered a memory of its own this run. With one
        # available the agent writes there instead, which is the whole reason
        # `--isolated` exists — so the answer is recorded, not assumed.
        rival_memory: (
            $events
            | where type == "system"
            | get -o 0.memory_paths
            | is-not-empty
        )
        answer: ($result | get -o 0.result | default "")
        cost_usd: ($result | get -o 0.total_cost_usd | default 0)
        turns: ($result | get -o 0.num_turns | default 0)
        session: $calls
    }
}

# The agmem tool calls in a session's event stream, in the order they happened.
def agmem-calls [events: list] {
    let assistant = ($events | where type == "assistant")
    if ($assistant | is-empty) {
        return []
    }
    $assistant
    | get message.content
    | flatten
    | where type == "tool_use"
    | where ($it.name | str starts-with "mcp__agmem__")
    | each {|call| {tool: ($call.name | str replace "mcp__agmem__" ""), input: $call.input}}
}

# Preload a store the way a previous session would have left it.
#
# Raw JSON-RPC over stdin rather than a client library: it is the same path a
# real session takes, and the process exits when stdin closes.
def seed [binary: string, data: string, cache: string, memories: list, overrides: record] {
    let request = {
        jsonrpc: "2.0"
        id: 1
        method: "initialize"
        params: {
            protocolVersion: "2025-06-18"
            capabilities: {}
            clientInfo: {name: "desc-eval", version: "1"}
        }
    }
    let call = {
        jsonrpc: "2.0"
        id: 2
        method: "tools/call"
        params: {name: "remember", arguments: {memories: $memories}}
    }
    let wire = (
        [$request {jsonrpc: "2.0", method: "notifications/initialized"} $call]
        | each {|message| $message | to json --raw}
        | str join "\n"
    )
    let seeded = (
        $wire
        | with-env (agmem-env $data $cache $overrides) { ^$binary --no-daemon | complete }
    )
    if $seeded.exit_code != 0 {
        error make {msg: $"seeding failed: ($seeded.stderr)"}
    }
    let answer = (
        $seeded.stdout
        | lines
        | each {|line| $line | from json}
        | where ($it.id? | default 0) == 2
    )
    if ($answer | is-empty) or (($answer | get -o 0.error) != null) {
        error make {msg: $"the store refused the seed: ($seeded.stdout)"}
    }
}

# Everything an agmem in this batch is started with.
def agmem-env [data: string, cache: string, overrides: record] {
    {AGMEM_DATA: $data, AGMEM_SPACE: "eval", FASTEMBED_CACHE_DIR: $cache} | merge $overrides
}

# The descriptions this binary serves, as `list_tools` reports them.
def descriptions [binary: string, cache: string, overrides: record] {
    let data = (mktemp -d)
    let wire = (
        [
            {
                jsonrpc: "2.0"
                id: 1
                method: "initialize"
                params: {
                    protocolVersion: "2025-06-18"
                    capabilities: {}
                    clientInfo: {name: "desc-eval", version: "1"}
                }
            }
            {jsonrpc: "2.0", method: "notifications/initialized"}
            {jsonrpc: "2.0", id: 2, method: "tools/list"}
        ]
        | each {|message| $message | to json --raw}
        | str join "\n"
    )
    $wire
    | with-env (agmem-env $data $cache $overrides) {
        ^$binary --no-daemon --embedder none | complete
    }
    | get stdout
    | lines
    | each {|line| $line | from json}
    | where ($it.id? | default 0) == 2
    | get 0.result.tools
    | select name description
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
