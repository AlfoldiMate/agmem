<div align="center">

# agmem

**Memory for coding agents, over MCP.**

One local binary, one embedded database, no API keys, no server-side LLM.
The agent distils what is worth keeping; agmem stores it, dates it, ranks it,
and shows its work.

[![CI](https://github.com/AlfoldiMate/agmem/actions/workflows/ci.yml/badge.svg)](https://github.com/AlfoldiMate/agmem/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/AlfoldiMate/agmem?display_name=tag)](https://github.com/AlfoldiMate/agmem/releases/latest)
[![MSRV](https://img.shields.io/badge/rust-1.89%2B-orange)](Cargo.toml)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#license)

[Quick start](#quick-start) · [What you get](#what-you-get) · [The memory model](#the-memory-model) · [The tools](#the-tools) · [Cookbook](#cookbook) · [Configuration](#configuration) · [Development](#development)

</div>

---

## A session, with memory

Every session of a coding agent starts from zero: the project's conventions,
your preferences, the lesson that cost an hour yesterday, all gone. With agmem
installed, a session opens like this instead:

```markdown
# Memory context (spaces: atlas + user)

## Instructions
- Never force-push to main `01M14XWWAXJG…`

## Profile
- The user prefers Go over Python for command-line tools `01M14XY6T7RX…`

## Relevant
- atlas deploys with bin/ship.sh; the old `make deploy` target is gone `01M16TXAAD2K…`
- Branch feat/retry-queue: the channel version replaced the mutex one after it
  deadlocked under the retry path; next is the backoff test `01M1QP2RV8NA…`

## Lessons
- The build breaks on a cold cargo cache `01M14Y0PB3WD…`
```

That block is put in front of the model before its first token, aimed at the
branch you are on. While the agent works it **recalls** in words before it
assumes something, and at a seam — a decision made, a push, a question you
answered — it **remembers** one claim at a time. When the context gets long,
a hook says so, and `/agmem:checkpoint` writes the session's durable state
back to the store:

```
Stored 4 claims (3 fact, 1 lesson), 1 superseded; left out the diff, git has it.
You can /clear now — the next session starts with this in front of it.
```

Then `/clear`, and the next session picks up where this one left off, with no
compaction and no re-explaining. That is the whole loop.

What makes it different from a hosted memory layer:

- **Local and offline.** SurrealDB embedded in the process, an embedding model
  on llama.cpp fetched on first run. Nothing leaves the machine.
- **The agent is the author.** agmem never rewrites a claim. It stores what it
  is given, reports duplicates and neighbours, and lets the agent decide.
- **Corrections, not contradictions.** A wrong claim is superseded, never
  overwritten. The old one stays readable and dated; one claim is live.
- **Every answer explains itself.** Recall hits carry the signals behind their
  rank. Pages say what they cut. Deletions confirm their scope first.
- **One store, many sessions.** A shared daemon serves every window, worktree
  and project on the machine, and a `ws://` URL shares it across machines.

## Quick start

Three commands on macOS (Apple silicon) or Linux (arm64, x86_64, glibc 2.38+).

```sh
brew install AlfoldiMate/tap/agmem                  # 1. the binary
agmem --doctor                                      # 2. self-check; downloads the model once (~314 MB)
claude plugin marketplace add AlfoldiMate/agmem \
  && claude plugin install agmem@agmem              # 3. the Claude Code plugin
```

Open Claude Code in any project. The first session starts with an empty
briefing and a nudge or two; run `/agmem:checkpoint` before you close it, and
the second session opens with what the first one learned.

<details>
<summary>What <code>--doctor</code> prints, and where the data lives</summary>

```
$ agmem --doctor
  ok    data dir writable    ~/Library/Application Support/dev.agmem.agmem
  ok    tool descriptions    agmem's own wording
  ok    shared daemon        not running; the next session starts one
  ok    single-writer lock   held by this process
  ok    database open        surrealkv://…/agmem.db
  ok    schema               v9
  ok    write/read roundtrip scratch record created and removed
  ok    embedder             embeddinggemma-300m-q8 (768d)
  ok    embedder vs store    same model and width
  ok    vector coverage      every row carries a vector
doctor: all checks passed
```

The report goes to stderr and the exit status is 0 only when every line
passed, so it doubles as a setup gate. `--model bge-small-en-v1.5` picks the
36 MB light option if the download or the machine is the constraint. On Apple
silicon the model runs on Metal; elsewhere on the CPU.

| Platform | Data directory |
|---|---|
| macOS | `~/Library/Application Support/dev.agmem.agmem` |
| Linux | `~/.local/share/agmem` |
| Windows | `%APPDATA%\agmem\agmem\data` |

One directory holds the store, the model cache, the lock file and the daemon
socket. Back it up, move it or delete it as a unit. `--data` or `AGMEM_DATA`
points it elsewhere.

</details>

<details>
<summary>Building from source instead of Homebrew</summary>

Rust 1.89 or newer, `cmake` and a C++ compiler; llama.cpp is compiled into the
binary. Nothing is on crates.io; the crate is `agmem-server` and the binary it
produces is `agmem`.

```sh
cargo install --git https://github.com/AlfoldiMate/agmem agmem-server
```

**Windows** has no prebuilt binary, so this is the install path there. The
toolchain is the MSVC one: install [Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/)
with the *Desktop development with C++* workload, and CMake, then the same
`cargo install` from a Developer PowerShell. The model runs on the CPU, the
data directory is `%APPDATA%\agmem\agmem\data`, and `agmem --doctor` is the
same self-check.

```powershell
winget install Kitware.CMake Rustlang.Rustup
cargo install --git https://github.com/AlfoldiMate/agmem agmem-server
```

</details>

<details>
<summary>Not using Claude Code?</summary>

The binary is a stdio MCP server; any client registers it with no arguments.
Desktop apps do not inherit your shell's `PATH`, so give them the absolute
path from `which agmem`.

```json
{ "mcpServers": { "agmem": { "command": "/opt/homebrew/bin/agmem" } } }
```

Without a project directory, set `AGMEM_SPACE=user` so the session serves
your cross-project space. The briefing is also a shell command, `agmem
context`, so any client with a session-start hook can inject it the way the
plugin does — see the [cookbook](#cookbook).

</details>

## What you get

| | |
|---|---|
| **A briefing, not a search box** | Every session opens with a character-budgeted block of what the store knows: standing instructions, your profile, what is relevant to this branch and commit, the lessons. Each line ends in the id of the claim it came from, so the agent can correct it rather than work around it. |
| **Recall that shows its work** | Ask in words. Full-text and vector search fused, then rescored by how recent and how used each claim is. Every hit carries its signals; a low-ranked answer says why it is low, and a full page says what it cut. |
| **Corrections with a paper trail** | A claim that fixes an older one names it in `supersedes`. The old one closes but stays readable and dated. `inspect` walks the chain, the episode a claim came from, or everything ever said about an entity. |
| **Memory that fades unless it is used** | A `fact` fades in weeks, a `lesson` in months, an `instruction` never and is pinned into every briefing. Branch state is a fast-decaying fact tagged with the branch, so it dies on its own once the branch is gone. |
| **Documents, not walls of text** | A plan, a review, a test log is stored whole, versioned by title, readable a chunk at a time and addressable by id. A subagent hands back `DOC: <id>` instead of its output, and a recall hit that lands inside a document links to it. |
| **Spaces that need no configuring** | Each session derives its space from the enclosing git project, so every branch and worktree of a repo shares one store, and a `user` space holds what follows you across projects. |
| **Nothing to run** | The first session starts a small daemon that owns the store; later sessions attach; the last one lets it expire. A `ws://` URL points every machine at one SurrealDB server instead. |
| **Retrieval that is measured** | An offline quality eval rides `cargo test` against a recorded baseline, and an LLM-driven harness measures whether a tool's description gets it called. Ranking and wording changes pass the numbers, not taste. |
| **Hooks for the seams** | The plugin nudges once after a `git push`, once after a question you answered, once when a session recalled and wrote nothing, and from 120k tokens of context says "checkpoint, then clear". |

## The memory model

A memory is one atomic claim, in the third person, written so it still makes
sense with no conversation around it. *"The user prefers Go over Python for
command-line tools"*, not *"switched to Go"*. The agent does the distilling;
agmem stores what it gets and reports back what it already held.

| Field | Values | Effect |
|---|---|---|
| `kind` | `fact`, `lesson`, `instruction`, `summary` | Which briefing section it lands in. `instruction` is pinned into every briefing, so be sparing. |
| `decay_class` | `pinned`, `slow`, `normal`, `fast` | How fast it fades from ranking. `fast` is closed at startup after about twenty idle days. |
| `entities`, `tags` | free text | Filters for `recall`, subjects for `inspect`, the hop seed for multi-hop recall. |
| `episode` | verbatim text | Stored unedited, chunked, and provenanced to every claim in the same call. |
| `supersedes` | ids | Closes those claims as corrected by this one. |

### A round trip

Store a claim:

```json
{ "memories": [
    { "content": "The user prefers Rust over Python for command-line tools",
      "kind": "fact", "entities": ["user"], "tags": ["identity"] } ] }
```

```json
{ "created": ["01M14XWWAXJG…"], "duplicates": [], "superseded": [], "related": [] }
```

Send it again, reworded, and nothing is written. The stored claim comes back
with its text, so a correction that reads like a duplicate can be recognised
as one:

```json
{ "created": [],
  "duplicates": [ { "id": "01M14XWWAXJG…", "of": 0, "similarity": 0.983,
                    "content": "The user prefers Rust over Python for command-line tools" } ] }
```

Correct it. The old claim stays readable and dated; only the new one is live.
`supersedes` takes a list, so one call also merges a duplicate cluster:

```json
{ "memories": [
    { "content": "The user prefers Go over Python for command-line tools",
      "supersedes": ["01M14XWWAXJG…"] } ] }
```

Ask for it back:

```json
{ "query": "what language does the user want for command-line tools?", "k": 5 }
```

```json
{ "hits": [ { "id": "01M14XY6T7RX…", "kind": "fact", "space": "myproject",
              "content": "The user prefers Go over Python for command-line tools",
              "score": 0.925,
              "signals": { "rrf_normalized": 1.0, "retention": 1.0, "importance": 0.5 } } ] }
```

Over weeks, claims decay unless they are used, and `consolidate` lists what
needs tidying — near duplicates, contradictions, stale notes — and does
nothing about them until the agent decides.

## The tools

Seven tools. Two MCP prompts, which Claude Code shows as slash commands, ask
for them at the right moments.

| Tool | Does |
|---|---|
| `remember` | Store distilled claims, optionally with the verbatim episode they came from. An episode given a `title` and `doc_kind` is a document: a plan, review, report, probe or transcript kept whole, versioned by title. Answers with a diff: created, duplicates, superseded, related. |
| `recall` | Ask in words. BM25 and vector search fused, rescored by retention. Every hit carries its signals; a full page says what it cut. |
| `context` | The session-start block: Instructions, Profile, Relevant, Lessons, within a character budget, every line ending in its id. |
| `inspect` | The paper trail: a claim's correction chain, the episode behind it, everything ever said about an entity, a document by title read one chunk at a time, the documents a space holds, or per-space counts. |
| `reflect` | Store a conclusion with the ids it was drawn from. A `summary` stands in for its cited claims when `context` runs short of budget. |
| `forget` | Close a memory, or purge it. By query it takes two identical calls, the first a dry run, so a deletion never reaches something that merely resembles the request. A document is not purged while live claims cite it, unless `cascade` takes them with it. CLI-first (`agmem forget <id>…`); on MCP with `AGMEM_TOOLS=all`. |
| `consolidate` | What needs tidying and nothing done about it: near-duplicate groups, contradiction candidates, stale working notes, over-full tags, documents nothing cites. Full text on every candidate. CLI-first (`agmem consolidate`); on MCP with `AGMEM_TOOLS=all`. |

| Prompt | Asks the agent to |
|---|---|
| `recall_first` | Read the memory block before the first move, and correct it rather than work around it |
| `checkpoint` | Review the session, recall each candidate to find what it corrects, then write the batch with `supersedes` on the corrections |

The same binary is the shell side: `agmem context` prints the briefing,
`agmem doc` stores and reads documents, `agmem consolidate` and `agmem forget`
are the tidy-up pair, `agmem hook <event>` answers Claude Code's hook events.
`agmem --help` has everything.

## Cookbook

Recipes for wiring agmem into a config and a workflow, from the smallest to
the whole framework. Each one stands alone.

### The plugin, and nothing else

The Claude Code plugin is the complete wiring: it registers the server, injects
the briefing at session start, nudges at the seams, ships `/agmem:checkpoint`,
`/agmem:memory` and `/agmem:doctor`, and logs which claims each session
recalled and wrote. Every hook is `agmem hook <event>`, so it needs nothing
beyond the binary. The plugin's version tracks the binary's; upgrade both
together. [plugin/README.md](plugin/README.md) describes each piece.

```sh
claude plugin marketplace add AlfoldiMate/agmem
claude plugin install agmem@agmem
```

Do not also `claude mcp add agmem` by hand. Both stay configured but Claude
Code connects only one, and the winner decides the tool names.

### The checkpoint-then-clear habit

Compaction throws away the reasoning and keeps the noise. The habit that
replaces it: at a seam, `/agmem:checkpoint`, then `/clear`, and the next
session opens briefed. The plugin says so from 120k tokens of context and
again per further 40k; move the thresholds, or turn the nudge off:

```sh
export AGMEM_CONTEXT_NUDGE_TOKENS=80000   # first nudge
export AGMEM_CONTEXT_NUDGE_STEP=20000     # then every N tokens after; 0 turns it off
```

Prefer several short sessions chained through memory over one long one; cache
reads scale with context × turns, so the long session is also the expensive
one.

### Branch state that dies with the branch

What is done and verified, the immediate next action, what is blocked on what:
`fact`s with `decay_class: fast`, tagged `branch:<slug>`. The briefing at
session start names the tag, and the state fades in days, so nothing stale
survives a merged branch.

```json
{ "memories": [
    { "content": "Branch feat/retry-queue: the backoff test is next; the channel version replaced the mutex one after it deadlocked under the retry path",
      "kind": "fact", "decay_class": "fast", "tags": ["branch:feat-retry-queue"] } ] }
```

Recall with the tag when resuming; `agmem doc list --tag branch:<slug>` lists
the documents that carry it.

### A personal space that follows you

Each session writes to the project's space and reads it *and* `user`.
Preferences and standing rules go to `user` explicitly, and every project's
briefing carries them:

```json
{ "space": "user",
  "memories": [
    { "content": "The user wants commit messages that say why, never what the diff already shows",
      "kind": "instruction" } ] }
```

`consolidate` looks in the current space alone, so a tidy-up never reaches the
shared space unasked. Derivation never lands on `user`; only an explicit
`AGMEM_SPACE=user` serves personal memory as a session's own space.

| `space` | Means |
|---|---|
| omitted | Write to this server's space; read it **and** `user` |
| `current` | This server's space |
| `user` | The cross-project space. Writes there must say so. |
| `all` | Every registered space, read only |
| a name | That space |

### Subagents hand back an address, not a wall of text

A subagent has a shell before it has MCP tools. Its long output — a plan, a
review, a test log — goes to a document, and the main thread gets the id:

```sh
agmem doc put --title plan-retry-queue --kind plan --tag role:architect < plan.md
# 01K4…  memory://atlas/doc/01K4…
```

The agent's return contract ends `DOC: <id> <uri>`; whoever needs the body
reads it a chunk at a time:

```sh
agmem doc get plan-retry-queue --raw            # the newest version under the title
agmem doc get 01K4… --offset 0 --limit 4000     # a window, by id
agmem doc list --kind plan                      # what the space holds
agmem doc forget 01K4… --purge [--cascade]      # gone, with or without the claims that cite it
```

A second `put` under the same title is a new version; every version stays
readable by id. The document is also an MCP resource at
`memory://<space>/doc/<id>`, so a client's `@` picker attaches it like a file.

### Your own session-start hook

Any client with a hook can inject the briefing the way the plugin does. The
shell command prints the same block the MCP tool returns, attaching to the
running daemon or starting the one the session is about to reuse:

```sh
agmem context --query "$(git log -1 --format=%s)" --budget-chars 4000
```

A Claude Code `SessionStart` hook that does this by hand is one line in
`settings.json`; the plugin's [hooks.json](plugin/hooks/hooks.json) is the
reference. If a project's own hook already injects `agmem context`, remove it
and keep the plugin's, or the briefing appears twice.

### The tidy week

Once in a while, list what needs attention and decide:

```sh
agmem consolidate                          # duplicate groups, contradictions, stale notes, orphan documents, as JSON
agmem forget 01K4… memory:01K4… --dry-run  # what those ids select
agmem forget 01K4…                         # close it; --purge deletes outright
```

`/agmem:memory tidy` runs the same loop from inside a session, merging
duplicate clusters through `remember` with `supersedes` and closing the rest.
`/agmem:memory show` prints what the store holds and judges whether the
briefing is still right.

### One store for the team

Point every agmem at a SurrealDB server instead of the embedded file:

```sh
surreal start --bind 127.0.0.1:8000 --unauthenticated surrealkv://~/surreal/agmem.db
```

```json
{ "mcpServers": { "agmem": {
    "command": "agmem",
    "env": { "AGMEM_DB": "ws://memory.internal:8000" }
} } }
```

The server is then the single-writer boundary and the security boundary. agmem
has no auth model of its own: spaces are scopes, not permissions, so anyone
who can reach the server reads every space. Keep it on a trusted network, and
use `AGMEM_DB_USER` and `AGMEM_DB_PASS` as a pair for an authenticated server.

### A small machine, or no local model

The light model is a fifth of Gemma's CPU latency and 36 MB:

```sh
export AGMEM_MODEL=bge-small-en-v1.5
```

A host that cannot run a model at all embeds through any OpenAI-compatible
endpoint — Ollama and `llama-server` locally, OpenAI or Voyage remotely:

```sh
export AGMEM_EMBEDDER=api
export AGMEM_API_URL=http://localhost:11434/v1
export AGMEM_API_MODEL=nomic-embed-text
# AGMEM_API_KEY=… for a remote endpoint; never a flag, never logged
```

The configured model wins: a store holding another model's vectors is moved
on the next start and re-embedded in the background while it already serves.
`agmem reindex` does it in one sitting with no session attached.

### Your own tool wording

A tool's description is most of what decides whether an agent reaches for it.
Replace one per server, with no rebuild:

```sh
export AGMEM_TOOL_DESC_RECALL="Before assuming anything about this project's conventions, ask here first."
```

[docs/tool-descriptions.md](docs/tool-descriptions.md) records the measured
effect of the built-in wording and the harness that measures it, so a
rewording can be checked rather than believed.

### The whole workflow: ctx-flow

[ctx-flow](https://github.com/AlfoldiMate/ctx-flow) is a token-frugal
`.claude` folder built around agmem: *the main thread holds decisions;
everything else holds output.* Searches, suites, logs, browsers and trackers
route into subagents with return contracts that end in `DOC: <id>`; a hook
denies the whole-file reads that flood a context; `/ctx-checkpoint` writes to
agmem at a seam and gates the lessons subagents proposed; `/ctx-onboard` grows
the seed into your project's own workflow from a scan of the repo and your
past sessions. It is the framework agmem is developed with, and the first
consumer of the plugin.

```sh
git clone https://github.com/AlfoldiMate/ctx-flow /path/to/your-project/.claude
rm -rf /path/to/your-project/.claude/.git     # it is your project's folder now
```

The `main` branch is the seed; `sln-rust-nu-fresh-proj` is what onboarding
produced for a fresh Rust + Nushell project, usable as-is for such a project
or as the worked example for another stack. Its README lists the tools it
routes and how to add one.

## Configuration

Every flag has an environment variable. `agmem --help` has the exact spellings.

| Flag / env | Default | Meaning |
|---|---|---|
| `--data` / `AGMEM_DATA` | platform data dir | Store, lock file, model cache |
| `--db` / `AGMEM_DB` | `surrealkv://<data>/agmem.db` | Engine. `mem://` for scratch, `ws://host` to share |
| `--space` / `AGMEM_SPACE` | derived from cwd | This instance's space |
| `--embedder` / `AGMEM_EMBEDDER` | `llama` | The local llama.cpp runtime. `api` embeds through an OpenAI-compatible endpoint instead |
| `--model` / `AGMEM_MODEL` | `embeddinggemma-300m` | Or `bge-small-en-v1.5`, the light option. The configured model wins: a store holding another model's vectors is moved on open and re-embedded in the background |
| `--accelerator` / `AGMEM_ACCELERATOR` | `auto` | `metal` on Apple silicon, `cpu` anywhere, or to opt out |
| `--api-url` / `AGMEM_API_URL` | `https://api.openai.com/v1` | For `--embedder api`: `POST <url>/embeddings`. OpenAI, Voyage, Ollama, vLLM and `llama-server` all answer it |
| `--api-model` / `AGMEM_API_MODEL` | `text-embedding-3-small` | For `--embedder api`: the remote model. Its width is learnt at startup; its thresholds are unmeasured and carry EmbeddingGemma's |
| `AGMEM_API_KEY` | unset | For `--embedder api`: the bearer token. Never a flag, never logged; a local endpoint needs none |
| `--pool` / `AGMEM_POOL` | 64 | Candidate pool before rescoring |
| `--max-k` / `AGMEM_MAX_K` | 50 | Ceiling for `recall`'s `k` |
| `--idle-timeout` / `AGMEM_IDLE_TIMEOUT` | 600 | Seconds the daemon outlives its last session; `0` keeps it up |
| `--no-daemon` / `AGMEM_NO_DAEMON` | off | Own the store in this process, one session at a time |
| `--log`, `--log-file` | `info` to stderr | Telemetry. stdout is the MCP wire and stays empty |
| `--tools` / `AGMEM_TOOLS` | `core` | Which tools a session lists: `core` leaves out `consolidate` and `forget` (the shell serves them), `all` puts them back |
| `AGMEM_TOOL_DESC_<TOOL>` | built-in wording | Replace one tool's description, per server, no rebuild |
| `AGMEM_CONTEXT_NUDGE_TOKENS`, `_STEP` | 120000, 40000 | The plugin's context-size nudge: first at, then per; `0` turns it off |
| `AGMEM_MODEL_DIR` | `<data>/models` | Where the model weights live |
| `--doctor` | | Self-check, then exit. Counts documents per space |
| `reindex` subcommand | | Re-embed every row now, with no session attached; `--model` picks the target for this run. Refuses while a daemon serves the store |

## Troubleshooting

- **`another agmem process (pid N) already owns the data dir`.** That pid is
  usually the daemon, and the cause is `--no-daemon` on a session that could
  have attached. Drop the flag or stop the process.
- **A session came up with no memory tools.** Read `<data dir>/daemon.log`.
  The shared store failed to start, and the session refused rather than open
  a second copy of a single-writer store.
- **Claude Desktop shows the server as failed.** Almost always `PATH`. Use the
  absolute path to the binary.
- **`--doctor` says `skip` on two lines.** Healthy, with a daemon running. The
  lock and the schema belong to the daemon. Stop the sessions for the full
  report.
- **The first run stalls on the model download.** It pulls ~314 MB from
  Hugging Face into `<data dir>/models`. Behind a proxy, copy that directory
  from a machine that has it, or point `AGMEM_MODEL_DIR` at one that does.
  `--model bge-small-en-v1.5` is 36 MB.
- **A different model.** The configured model wins. A store written with
  another model, which includes every store from before v0.3, is moved on the
  next start: vectors cleared, indexes resized, rows re-embedded in the
  background while the store already serves. Until the last row is done,
  every tool result ends with a line saying how many are left; recall sees
  them through BM25 meanwhile. `agmem reindex` needs the store to itself, so
  end the sessions first. There is no model-less mode: recall is BM25 *and*
  vectors.
- **Starting over.** Delete the data directory. Keep `models/` to skip the
  download.

## Development

### Building and testing

Rust 1.89 or newer, `cmake` and a C++ compiler; llama.cpp is compiled into
the binary on the first build, so expect a few minutes then.

```sh
git clone https://github.com/AlfoldiMate/agmem
cd agmem
cargo test --workspace                                   # unit, integration and the offline quality eval
cargo clippy --workspace --all-targets -- -D warnings
```

Four crates: `agmem-core` (records, scoring, dedup, chunking; no I/O),
`agmem-store` (SurrealDB schema and queries), `agmem-embed` (llama.cpp, API
and no-op backends) and `agmem-server` (the MCP service and the `agmem`
binary). CI never downloads a model: tests that need real semantics replay
recorded model vectors from `tests/fixtures/`, and tests that need the live
model are `#[ignore]`d.

Retrieval quality is measured, not asserted. An offline, deterministic eval
rides `cargo test` against a recorded baseline in
[docs/eval/quality.md](docs/eval/quality.md), and an LLM-driven harness in
`scripts/desc-eval.nu` measures whether a tool description gets the tool
called. Both are the gate for changes to ranking or wording.

### Working in this repo with Claude Code

This repository is developed with [ctx-flow](https://github.com/AlfoldiMate/ctx-flow),
and the `.claude/` folder is not tracked here: it is a checkout of ctx-flow's
`sln-rust-nu-fresh-proj` branch, dropped into the working tree and gitignored,
so the framework has one home and every project that uses it stays current.

```sh
git clone --branch sln-rust-nu-fresh-proj https://github.com/AlfoldiMate/ctx-flow .claude
claude mcp add nu -- nu --mcp
claude plugin marketplace add AlfoldiMate/agmem && claude plugin install agmem@agmem
```

The framework assumes `nu`, `ast-grep`, `gh` and `rtk` on `PATH` (its README
has the table and the one-line installs), and without them the hooks fail
silently. Open Claude Code in the checkout and run `/ctx-doctor`: it checks
each dependency, both MCP registrations, the hooks, and whether ast-grep
parses Rust here, and prints the exact fix for anything missing. `/ctx-grammar
nu` teaches ast-grep the Nushell grammar the hooks are written in; it writes a
machine-local `sgconfig.yml` at the repo root, which is gitignored.

Two things not to do: register agmem by hand with `claude mcp add` (the
plugin's registration would lose, and the renamed tools leave the framework's
agents without memory), and `rtk init -g` (the framework registers rtk's hook
itself; a second registration rewrites every command twice).

The plugin under `plugin/` is not enabled by anything in the repo; sessions
here use it the way any user does, installed at user scope, so a plugin change
is dogfooded once it is released. `claude --plugin-dir ./plugin` loads the
working-tree plugin without installing it.

**Worktrees.** The maintainer's checkout is a bare repository with one sibling
directory per branch, driven by [Nustro](https://github.com/AlfoldiMate/Nustro)'s
`worktree` command. The root holds `.claude` as a symlink to the ctx-flow
checkout, and `worktree apply` places it into each worktree along with the
machine-local files from `.profiles/` — the local settings, `sgconfig.yml` —
which is why `.gitignore` lists `.claude` without a trailing slash (a symlink
does not match a dir pattern) and why the framework denies a raw `git worktree
add` in that layout. A plain clone needs none of this.

### Contributing and releasing

`main` is protected and PR-only. CI runs fmt, clippy with warnings denied, and
the full suite on Linux and macOS.

A release is what happens after a merge. Once CI passes on `main`, the
`release-tag` workflow opens a version-bump PR that merges itself, and that
merge pushes the tag. The tag fires cargo-dist, which builds every target,
publishes the GitHub release with build attestations, and updates the Homebrew
tap. The plugin's manifests are pinned to the same version in the bump.

The bump size follows the PR: an ordinary merge is a patch; a PR labelled
`release:minor` is a minor; `release:skip` releases nothing. A PR attached to
an open milestone does not release on its own — closing the milestone ships
everything since the last tag as one minor. Anything merged in the meantime
still rides the next patch release, so keep milestone work on its branch if it
must not leak early. `workflow_dispatch` on `release-tag` forces a bump by
hand.

## Docs

- [docs/design.md](docs/design.md) — architecture, schema, tool contracts, retrieval and decay
- [docs/tool-descriptions.md](docs/tool-descriptions.md) — what the descriptions say, and the measured effect
- [docs/eval/](docs/eval/) — quality baseline, fusion sweep, rerank and NLI probes, embedding model measurements
- [docs/idea.md](docs/idea.md) — the research this is built on
- [plugin/README.md](plugin/README.md) — the Claude Code plugin, piece by piece
- [ctx-flow](https://github.com/AlfoldiMate/ctx-flow) — the framework this repo is developed with

## License

MIT or Apache-2.0, at your option.
