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

[Install](#install) · [How it works](#how-it-works) · [The tools](#the-tools) · [Spaces](#spaces) · [Configuration](#configuration) · [Development](#development) · [Docs](#docs)

</div>

---

## Why

Every session of a coding agent starts from zero. Project conventions, the
user's preferences, the lesson that cost an hour yesterday: all gone. Most
memory layers fix this with a hosted service and a second model that rewrites
what the agent said. agmem does neither.

- **Local and offline.** SurrealDB embedded in the process, an embedding
  model on llama.cpp fetched on first run. Nothing leaves the machine.
- **The agent is the author.** agmem never rewrites a claim. It stores what it
  is given, reports duplicates and neighbours, and lets the agent decide.
- **Corrections, not contradictions.** A wrong claim is superseded, never
  overwritten. The old one stays readable and dated; one claim is live.
- **Every answer explains itself.** Recall hits carry the signals behind their
  rank. Pages say what they cut. Deletions confirm their scope first.
- **One store, many sessions.** A shared daemon serves every window, worktree
  and project on the machine, and a `ws://` URL shares it across machines.

## Install

Three steps: the binary, a self-check, and the Claude Code plugin that wires
it into every session.

### 1. The binary

Homebrew is the supported path on macOS (Apple silicon) and Linux (arm64 and
x86_64, glibc 2.38+). The tap is updated by every release.

```sh
brew install AlfoldiMate/tap/agmem
```

Anywhere Homebrew does not reach, build from source. You need Rust 1.89 or
newer, `cmake` and a C++ compiler, because llama.cpp is compiled into the
binary. Nothing is on crates.io; the crate is `agmem-server` and the binary
it produces is `agmem`.

```sh
cargo install --git https://github.com/AlfoldiMate/agmem agmem-server
```

### 2. The self-check

Run it once. It creates the data directory, opens the store, runs migrations,
does a write/read roundtrip and downloads the embedding model,
EmbeddingGemma-300M at Q8_0, about 314 MB. Every run after that is offline.
On Apple silicon the model runs on Metal; elsewhere on the CPU.

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
36 MB light option if the download or the machine is the constraint.

| Platform | Data directory |
|---|---|
| macOS | `~/Library/Application Support/dev.agmem.agmem` |
| Linux | `~/.local/share/agmem` |
| Windows | `%APPDATA%\agmem\agmem\data` |

One directory holds the store, the model cache, the lock file and the daemon
socket. Back it up, move it or delete it as a unit. `--data` or `AGMEM_DATA`
points it elsewhere.

### 3. The Claude Code plugin

The binary is an MCP server. Registering it alone gives the agent seven tools
and no reason to call them. The plugin gives it the reasons: it registers the
server, puts the memory briefing in front of the model before its first
token, nudges at the moments worth writing down, ships `/agmem:checkpoint`,
`/agmem:memory` and `/agmem:doctor`, and logs which claims each session
recalled and wrote. Every hook is a subcommand of the binary itself, so the
plugin needs nothing else installed.

```sh
claude plugin marketplace add AlfoldiMate/agmem
claude plugin install agmem@agmem
```

The plugin's version tracks the binary's; upgrade both together.
[plugin/README.md](plugin/README.md) describes each piece.

**A worked example of the wiring** is this repository's own `.claude/`
folder. It is the framework agmem is developed with, and the first consumer
of the plugin: agents that recall their own lessons, hooks that nudge at the
seams, a checkpoint command that decides what a session leaves behind. Read
[.claude/README.md](.claude/README.md) for the reasoning, and
[Development](#development) below for what it takes to run it.

**Any other MCP client** registers the binary as a stdio server with no
arguments: `{ "mcpServers": { "agmem": { "command": "agmem" } } }`. Desktop
apps do not inherit your shell's `PATH`, so give them the absolute path from
`which agmem`. Without a project directory, set `AGMEM_SPACE=user` so the
session serves the cross-project space. The briefing is also a shell command,
`agmem context --query "…" --budget-chars 4000`, so any client with a
session-start hook can inject it the way the plugin does.

## How it works

A session opens, and the plugin's hook asks agmem for a **briefing**: a
character-budgeted block of what the store knows, aimed at the current branch
and last commit. Instructions first, then the user's profile, then what is
relevant, then lessons. Every line ends in the id of the claim it came from.
The agent reads it and starts from there instead of from zero.

While it works, the agent **recalls** in words when it is about to assume
something, and the store answers with the claims that match, ranked by a
fusion of full-text and vector search and rescored by how recent and how used
each claim is. Every hit carries its signals, so a low-ranked answer explains
why it is low.

At a seam, a decision made, a push, a question answered, the agent
**remembers**: one atomic claim per entry, in the third person, so it still
makes sense with no conversation around it. The store embeds it, checks it
against what it holds, and answers with a diff: created, duplicate of, or
related to. A claim that corrects an older one names it in `supersedes`, and
the old one closes but stays readable. Nothing is ever rewritten by a model
you did not talk to.

Over weeks, claims **decay** unless they are used. A `fact` fades in weeks, a
`lesson` in months, an `instruction` never and is pinned into every briefing.
Branch state is a fast-decaying fact tagged with the branch, so it dies on its
own once the branch is gone. `consolidate` lists what needs tidying, near
duplicates, contradictions, stale notes, and does nothing about them until the
agent decides.

Long output has a home too. A plan, a review, a test log is a **document**:
stored whole, versioned by title, readable a chunk at a time, and addressable
by id. A subagent hands back `DOC: <id>` instead of a wall of text, and a
recall hit that lands inside a document links to it.

All of this sits in one directory under your home, served by a small daemon
the first session starts and the last session lets expire. There is nothing
to run, nothing to host, and nothing that phones out.

## The tools

Seven tools. Two MCP prompts, which Claude Code shows as slash commands, ask
for them at the right moments.

| Tool | Does |
|---|---|
| `remember` | Store distilled claims, optionally with the verbatim episode they came from. An episode given a `title` and `doc_kind` is a document: a plan, review, report, probe or transcript kept whole, versioned by title. Answers with a diff: created, duplicates, superseded, related. |
| `recall` | Ask in words. BM25 and vector search fused, rescored by retention. Every hit carries its signals; a full page says what it cut. |
| `context` | The session-start block: Instructions, Profile, Relevant, Lessons, within a character budget, every line ending in its id. |
| `forget` | Close a memory, or purge it. By query it takes two identical calls, the first a dry run, so a deletion never reaches something that merely resembles the request. A document is not purged while live claims cite it, unless `cascade` takes them with it. CLI-first (`agmem forget <id>…`); on MCP with `AGMEM_TOOLS=all`. |
| `inspect` | The paper trail: a claim's correction chain, the episode behind it, everything ever said about an entity, a document by title read one chunk at a time, the documents a space holds, or per-space counts. |
| `consolidate` | What needs tidying and nothing done about it: near-duplicate groups, contradiction candidates, stale working notes, over-full tags, documents nothing cites. Full text on every candidate. CLI-first (`agmem consolidate`); on MCP with `AGMEM_TOOLS=all`. |
| `reflect` | Store a conclusion with the ids it was drawn from. A `summary` stands in for its cited claims when `context` runs short of budget. |

| Prompt | Asks the agent to |
|---|---|
| `/mcp__agmem__recall_first` | Read the memory block before the first move, and correct it rather than work around it |
| `/mcp__agmem__checkpoint` | Review the session, recall each candidate to find what it corrects, then write the batch with `supersedes` on the corrections |

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

Start the next session with what is known:

```markdown
# Memory context (spaces: myproject + user)

## Instructions
- Never force-push to main `01M14XWWAXJG…`

## Profile
- The user prefers Go over Python for command-line tools `01M14XY6T7RX…`

## Lessons
- The build breaks on a cold cargo cache `01M14Y0PB3WD…`
```

### What a memory carries

| Field | Values | Effect |
|---|---|---|
| `kind` | `fact`, `lesson`, `instruction`, `summary` | Which `context` section it lands in. `instruction` is pinned into every briefing. |
| `decay_class` | `pinned`, `slow`, `normal`, `fast` | How fast it fades from ranking. `fast` is closed at startup after about twenty idle days. |
| `entities`, `tags` | free text | Filters for `recall`, subjects for `inspect`, the hop seed for multi-hop recall. |
| `episode` | verbatim text | Stored unedited, chunked, and provenanced to every claim in the same call. |
| `supersedes` | ids | Closes those claims as corrected by this one. |

### The shell side

Three things are easier from a shell than over MCP, and each is a subcommand
of the same binary.

**Documents.** A subagent has a shell before it has MCP tools, and a plan
handed around by id is a plan nobody has to find on disk:

```sh
agmem doc put --title plan-x --kind plan --tag role:architect < plan.md
# 01K4…  memory://myproject/doc/01K4…
agmem doc get plan-x --raw                 # the newest version, as stored
agmem doc get 01K4… --offset 0 --limit 4000
agmem doc list --kind plan
agmem doc forget 01K4… --purge [--cascade]
```

A second `put` under the same title is a new version; `get <title>` resolves
to the newest, and every version stays readable by id. The same document is
an MCP resource at `memory://<space>/doc/<id>`, so a client's `@` picker can
attach it like a file.

**Maintenance.** `consolidate` and `forget` are tidy-session verbs, so a
session lists five tools by default and these two live in the shell
(`AGMEM_TOOLS=all` puts them back on the wire):

```sh
agmem consolidate [--space user]           # the tool's JSON: what needs tidying
agmem forget 01K4… memory:01K4… --dry-run  # what those ids select, as JSON
agmem forget 01K4…                         # close it; --purge deletes outright
```

**The briefing.** `agmem context --query "release work" --budget-chars 4000`
prints the same block the MCP tool returns, attaching to the running daemon
or starting the one the session is about to reuse.

## Spaces

No space needs configuring. Each session derives one from where it runs: the
enclosing git project's name, so every worktree of a repo shares a space,
else the directory name. Set `AGMEM_SPACE` only to pin a name the folder does
not already say.

| `space` | Means |
|---|---|
| omitted | Write to this server's space; read it **and** `user` |
| `current` | This server's space |
| `user` | The cross-project space. Writes there must say so. |
| `all` | Every registered space, read only |
| a name | That space |

Derivation never lands on `user`; only an explicit `AGMEM_SPACE=user` serves
personal memory. `consolidate` looks in the current space alone, so a tidy-up
never reaches the shared space unasked.

## Sharing one store

**Several sessions, one machine.** The embedded store is single-writer, so the
first session starts a small daemon that owns it and later sessions attach.
Nothing to install or start. The daemon exits ten minutes after the last
session detaches; `AGMEM_IDLE_TIMEOUT=0` keeps it up, `--no-daemon` goes back
to one process per store.

**Several machines.** Point every agmem at a SurrealDB server instead:

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

## Configuration

Every flag has an environment variable. `agmem --help` has the exact spellings.

| Flag / env | Default | Meaning |
|---|---|---|
| `--data` / `AGMEM_DATA` | platform data dir | Store, lock file, model cache |
| `--db` / `AGMEM_DB` | `surrealkv://<data>/agmem.db` | Engine. `mem://` for scratch, `ws://host` to share |
| `--space` / `AGMEM_SPACE` | derived from cwd | This instance's space |
| `--embedder` / `AGMEM_EMBEDDER` | `llama` | The local llama.cpp runtime. `api` embeds through an OpenAI-compatible endpoint instead, for a host that cannot run the model |
| `--model` / `AGMEM_MODEL` | `embeddinggemma-300m` | Or `bge-small-en-v1.5`, the light option. The configured model wins: a store holding another model's vectors is moved on open and re-embedded in the background |
| `--accelerator` / `AGMEM_ACCELERATOR` | `auto` | `metal` on Apple silicon, `cpu` anywhere, or to opt out |
| `--api-url` / `AGMEM_API_URL` | `https://api.openai.com/v1` | For `--embedder api`: `POST <url>/embeddings`. OpenAI, Voyage, Ollama, vLLM and `llama-server` all answer it |
| `--api-model` / `AGMEM_API_MODEL` | `text-embedding-3-small` | For `--embedder api`: the remote model. Its width is learnt at startup; its thresholds are unmeasured and carry EmbeddingGemma's |
| `AGMEM_API_KEY` | unset | For `--embedder api`: the bearer token. Never a flag, never logged; a local endpoint needs none |
| `--pool` / `AGMEM_POOL` | 64 | Candidate pool before rescoring |
| `--max-k` / `AGMEM_MAX_K` | 50 | Ceiling for `recall`'s `k` |
| `--idle-timeout` / `AGMEM_IDLE_TIMEOUT` | 600 | Seconds the daemon outlives its last session |
| `--no-daemon` / `AGMEM_NO_DAEMON` | off | Own the store in this process |
| `--log`, `--log-file` | `info` to stderr | Telemetry. stdout is the MCP wire and stays empty |
| `--tools` / `AGMEM_TOOLS` | `core` | Which tools a session lists: `core` leaves out `consolidate` and `forget` (the shell serves them), `all` puts them back |
| `AGMEM_TOOL_DESC_<TOOL>` | built-in wording | Replace one tool's description, per server, no rebuild |
| `AGMEM_MODEL_DIR` | `<data>/models` | Where the model weights live |
| `--doctor` | | Self-check, then exit. Counts documents per space |
| `reindex` subcommand | | Re-embed every row now, with no session attached; `--model` picks the target for this run. Refuses while a daemon serves the store |

A tool description is most of what decides whether an agent reaches for
memory. `AGMEM_TOOL_DESC_RECALL` and friends replace one outright, and
[docs/tool-descriptions.md](docs/tool-descriptions.md) records the measured
effect of the built-in wording and the harness that measures it.

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
  them through BM25 meanwhile. To do it in one sitting, `agmem reindex`
  (with `--model` to pick the target) needs the store to itself, so end the
  sessions first. There is no model-less mode: recall is BM25 *and* vectors.
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

The repository's `.claude/` folder is [ctx-flow](.claude/README.md), the
framework agmem is developed with. It loads with every Claude Code session
opened here, and it assumes a few tools are on `PATH`. Without them the hooks
fail silently, so install them first:

| Tool | Why | Install |
|---|---|---|
| [nu](https://www.nushell.sh) | runs every hook and script; also the shell MCP server | `brew install nushell` |
| agmem | memory, through the plugin from step 3 above | `brew install AlfoldiMate/tap/agmem` |
| [ast-grep](https://ast-grep.github.io) | syntax-aware code search | `brew install ast-grep` |
| [gh](https://cli.github.com) | GitHub from the shell | `brew install gh` |
| [rtk](https://github.com/rtk-ai/rtk) | compresses Bash output; the hook is already registered in `settings.json`, so no `rtk init` | `brew install rtk` |
| [playwright-cli](https://github.com/microsoft/playwright-cli) *(optional)* | browser driving, for the `browser` agent | `npm i -g playwright-cli` |
| acli *(optional)* | Jira, for the `tracker` agent | Atlassian's installer |

You do not need Nushell as your login shell; the hooks run under `nu` whatever
your terminal runs. Then register the two MCP servers the framework relies on.
The shell server is registered by hand, memory comes with the plugin:

```sh
claude mcp add nu -- nu --mcp
claude plugin marketplace add AlfoldiMate/agmem
claude plugin install agmem@agmem
```

Do not also register agmem by hand with `claude mcp add`. Claude Code
connects only one, yours would win, and that renames the tools so the
framework's agents start without memory. Do not run `rtk init -g` either;
the framework registers rtk's hook in its own `settings.json`, and a second
registration rewrites every command twice.

Open Claude Code in the checkout and run `/ctx-flow-doctor`. It checks each
dependency, both MCP registrations, the hooks, and whether ast-grep parses
Rust here, and prints the exact fix for anything missing. `/ast-grep-it nu`
teaches ast-grep the Nushell grammar the hooks are written in; it writes a
machine-local `sgconfig.yml` at the repo root, which is gitignored.

What the folder holds, in one breath: `CLAUDE.md` carries the routing rules,
`settings.json` registers the hooks, `agents/` holds seven scoped subagents,
`commands/` the five slash commands, `hooks/` and `scripts/` the Nushell
behind them, `skills/` the reference cards, and `notebook.md` is Claude's own
undistilled notes, loaded whole each session. The plugin under `plugin/` is
not enabled by anything in the repo; sessions here use it the way any user
does, installed at user scope, so a plugin change is dogfooded once it is
released. `claude --plugin-dir ./plugin` loads the working-tree plugin
without installing it.

**Worktrees.** The maintainer's checkout uses a bare repository with one
sibling directory per branch, managed by `/bare-worktree`. That command
symlinks the machine-local files each worktree needs, the local settings,
`sgconfig.yml`, a private skill, from a `.profiles/` directory beside the
bare repo, and the framework's hooks deny a raw `git worktree add` in that
layout because it would skip them. A plain clone needs none of this;
`/bare-worktree init` converts one if you want the same setup.

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
- [.claude/README.md](.claude/README.md) — ctx-flow, the framework this repo is developed with

## License

MIT or Apache-2.0, at your option.
