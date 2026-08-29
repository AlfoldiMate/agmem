#!/usr/bin/env nu

# Does an agent actually reach for agmem? (design §9 risk 4, issues #23 and #22)
#
# The tool descriptions and the rituals are the product surface: nothing else
# decides whether a model calls `recall` before answering or `remember` after
# learning something. That cannot be unit-tested, so this drives real headless
# Claude Code sessions against a throwaway store and counts what each one
# reached for.
#
# Each scenario runs in its own data dir and its own empty working directory,
# with `--strict-mcp-config` and no settings sources, so agmem is the only MCP
# server present and nothing in the developer's own configuration leaks in.
# Except in the `ritual` scenario, no turn mentions memory — asking for a
# `recall` and getting one measures instruction-following, not the description.
#
#     nu scripts/desc-eval.nu --label before --runs 3
#     nu scripts/desc-eval.nu --label after --runs 3
#     nu scripts/desc-eval.nu report before after
#
# Results land in `docs/eval/<label>/`: one JSON per run plus a summary the
# report subcommand renders. Sessions cost money — one per turn, per scenario,
# per `--runs`.

# The questions risk 4 actually asks, one scenario apiece.
#
# `want` passes when *any* of its tools was called (orientation is `context` or
# `recall`, either is the right instinct); `avoid` fails when any of its tools
# was called at all. A scenario with an empty `want` is testing restraint.
#
# `turns` is a list because a ritual is a slash command with nothing to act on
# until a session has happened. Every call records which turn made it.
# How much of a tool's answer each recorded call keeps. Enough to see whether
# a recall came back with hits; not so much that a saved run is a transcript.
const ANSWER_CHARS = 700

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
        turns: ["How do I deploy atlas?"]
        want: ["recall" "context"]
        avoid: []
    }
    {
        name: "store"
        asks: "does it write a durable preference down without being told to?"
        seed: []
        turns: ["Heads up for future work on this codebase: I want library crates to use thiserror for their error types and binaries to use anyhow, never the other way round. It has bitten us twice."]
        want: ["remember"]
        avoid: []
    }
    {
        name: "ritual"
        asks: "does the checkpoint prompt get the write the description could not?"
        seed: []
        # The first turn is `store` verbatim, so the pair is a controlled
        # comparison: same words, same conditions, one extra turn asking for
        # the ritual. `store` measures what a description wins on its own;
        # this measures what a prompt wins when somebody asks for it.
        turns: [
            "Heads up for future work on this codebase: I want library crates to use thiserror for their error types and binaries to use anyhow, never the other way round. It has bitten us twice."
            "/mcp__agmem__checkpoint"
        ]
        want: ["remember"]
        avoid: []
    }
    {
        name: "ritual_correct"
        asks: "does the checkpoint prompt get the supersedes the description could not?"
        seed: [
            {
                content: "The user formats Python with black."
                kind: "fact"
                tags: ["identity"]
            }
        ]
        # `correct` with the ritual added, the same way `ritual` is `store`
        # with the ritual added. The metric that matters here is `superseded`,
        # not `pass`: #23 measured `remember` called 3/3 in isolation and
        # `supersedes` set 0/3, so writing is not what is in doubt.
        turns: [
            "I have moved off black — everything is formatted with ruff format now, and black is uninstalled. Note that for later."
            "/mcp__agmem__checkpoint"
        ]
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
        turns: ["I have moved off black — everything is formatted with ruff format now, and black is uninstalled. Note that for later."]
        want: ["remember"]
        avoid: []
    }
    {
        name: "restraint"
        asks: "does it leave memory alone when there is nothing to remember?"
        seed: []
        turns: ["What is the capital of France? Answer in one word."]
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

    # A ritual is a slash command, and a slash command has nothing to act on
    # until a session has happened — so a scenario is a *list* of turns, run
    # through one resumed session. Single-turn scenarios keep the old flags
    # exactly, including `--no-session-persistence`, which is incompatible with
    # resuming and is why it is conditional.
    let conversation = (($scenario.turns | length) > 1)
    let session_id = (random uuid)
    let slash = if ($scenario.turns | any {|turn| $turn | str starts-with "/"}) {
        []
    } else {
        ["--disable-slash-commands"]
    }

    let events = (
        $scenario.turns
        | enumerate
        | each {|turn|
            let continuity = if not $conversation {
                ["--no-session-persistence"]
            } else if $turn.index == 0 {
                ["--session-id" $session_id]
            } else {
                ["--resume" $session_id]
            }
            let spoken = (
                ^claude -p $turn.item
                    --model $model
                    --output-format stream-json --verbose
                    --strict-mcp-config --mcp-config $config
                    --permission-mode bypassPermissions
                    --setting-sources ""
                    ...$continuity
                    ...$slash
                    ...$settings
                | complete
            )
            $spoken.stdout
            | lines
            | where ($it | str starts-with "{")
            | each {|line| $line | from json | insert turn $turn.index}
        }
        | flatten
    )
    let calls = (agmem-calls $events)
    let used = ($calls | get tool | uniq)
    let hit = (($scenario.want | is-empty) or ($scenario.want | any {|tool| $tool in $used}))
    let clean = ($scenario.avoid | all {|tool| $tool not-in $used})
    # One result event per turn; the last one is what the session ended up
    # saying, and the cost is all of them.
    let results = ($events | where type == "result")
    let result = ($results | last 1)

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
            | init-events
            | all {|system|
                $system.mcp_servers?
                | default []
                | any {|server| $server.name == "agmem" and $server.status == "connected"}
            }
        )
        # Whether the client offered a memory of its own this run. With one
        # available the agent writes there instead, which is the whole reason
        # `--isolated` exists — so the answer is recorded, not assumed.
        rival_memory: (
            $events
            | init-events
            | any {|system| $system.memory_paths? | is-not-empty}
        )
        # The store this run used, kept so a surprising result can be opened
        # and looked at rather than argued about. It is a temp dir, so it is
        # only good until the machine cleans up.
        data: $data
        answer: ($result | get -o 0.result | default "")
        cost_usd: (
            $results
            | each {|turn| $turn.total_cost_usd? | default 0}
            | math sum
        )
        agent_turns: ($result | get -o 0.num_turns | default 0)
        session: $calls
    }
}

# What each turn reported about itself at startup, one event per turn.
#
# Not every `system` event is an init: with slash commands enabled the client
# emits others that carry no `mcp_servers` at all, and a check written as
# "every system event says agmem is connected" fails on those — reporting a
# session that worked as one that had no server.
def init-events [] {
    $in | where type == "system" and ($it.subtype? | default "") == "init"
}

# The agmem tool calls in a session's event stream, in the order they happened.
def agmem-calls [events: list] {
    # What each call got back, by id. Without this a recall that returned
    # nothing and a recall whose answer the agent ignored look identical in the
    # record — and those are opposite findings.
    let answers = (
        $events
        | where type == "user"
        | each {|event| $event.message.content? | default []}
        | flatten
        | where ($it.type? | default "") == "tool_result"
        | reduce --fold {} {|block, acc|
            $acc
            | insert $block.tool_use_id (
                $block.content | to json --raw | str substring 0..$ANSWER_CHARS
            )
        }
    )

    $events
    | where type == "assistant"
    | each {|event|
        $event.message.content
        | where type == "tool_use"
        | where ($it.name | str starts-with "mcp__agmem__")
        | each {|call|
            {
                # Which turn reached for it. In a scenario whose last turn is a
                # ritual, this is the difference between the ritual working and
                # the agent having already written on its own.
                turn: $event.turn
                tool: ($call.name | str replace "mcp__agmem__" "")
                input: $call.input
                answer: ($answers | get -o $call.id | default "")
            }
        }
    }
    | flatten
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
