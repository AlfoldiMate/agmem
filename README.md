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

[Install](#install) · [Register](#register-with-your-client) · [The loop](#the-loop) · [Spaces](#spaces) · [Configuration](#configuration) · [Docs](#docs)

</div>

---

## Why

Every session of a coding agent starts from zero. Project conventions, the
user's preferences, the lesson that cost an hour yesterday: all gone. Most
memory layers fix this with a hosted service and a second model that rewrites
what the agent said. agmem does neither.

- **Local and offline.** SurrealDB embedded in the process, an ONNX embedding
  model cached on first run. Nothing leaves the machine.
- **The agent is the author.** agmem never rewrites a claim. It stores what it
  is given, reports duplicates and neighbours, and lets the agent decide.
- **Corrections, not contradictions.** A wrong claim is superseded, never
  overwritten. The old one stays readable and dated; one claim is live.
- **Every answer explains itself.** Recall hits carry the signals behind their
  rank. Pages say what they cut. Deletions confirm their scope first.
- **One store, many sessions.** A shared daemon serves every window, worktree
  and project on the machine, and a `ws://` URL shares it across machines.

## Install

Prebuilt binaries cover macOS on Apple silicon and Linux on arm64 and x86_64
(glibc 2.38+).

```sh
brew install AlfoldiMate/tap/agmem
```

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/AlfoldiMate/agmem/releases/latest/download/agmem-server-installer.sh | sh
```

Anywhere else, build from source with Rust 1.89 or newer. The crate is
`agmem-server`; the binary is `agmem`.

```sh
cargo install --git https://github.com/AlfoldiMate/agmem agmem-server
```

Then run the self-check once. It creates the data directory, opens the store,
runs migrations, does a write/read roundtrip and downloads the embedding model
(BGE-small-en-v1.5, quantised, 65 MB). Every run after that is offline.

```
$ agmem --doctor
  ok    data dir writable    ~/Library/Application Support/dev.agmem.agmem
  ok    tool descriptions    agmem's own wording
  ok    shared daemon        not running; the next session starts one
  ok    single-writer lock   held by this process
  ok    database open        surrealkv://…/agmem.db
  ok    schema               v9
  ok    write/read roundtrip scratch record created and removed
  ok    embedder             bge-small-en-v1.5-q (384d)
  ok    embedder vs store    same model and width
  ok    vector coverage      every row carries a vector
doctor: all checks passed
```

The report goes to stderr and the exit status is 0 only when every line
passed, so it doubles as a setup gate.

| Platform | Data directory |
|---|---|
| macOS | `~/Library/Application Support/dev.agmem.agmem` |
| Linux | `~/.local/share/agmem` |
| Windows | `%APPDATA%\agmem\agmem\data` |

One directory holds the store, the model cache, the lock file and the daemon
socket. Back it up, move it or delete it as a unit. `--data` or `AGMEM_DATA`
points it elsewhere.

## Register with your client

agmem is a stdio MCP server: `command: "agmem"`, no arguments. One global
registration covers every project.

**Claude Code** — the plugin does the whole wiring: it registers the server,
injects the memory briefing at every session start, ships `/agmem:checkpoint`,
`/agmem:memory` and `/agmem:doctor`, and logs which claims each session
recalled and wrote (`plugin/README.md` has the details):

```sh
claude plugin marketplace add AlfoldiMate/agmem
claude plugin install agmem@agmem
```

Or register the server alone and wire hooks yourself:

```sh
claude mcp add agmem --scope user -- agmem
```

**Cursor**, in `~/.cursor/mcp.json`:

```json
{ "mcpServers": { "agmem": { "command": "agmem" } } }
```

**Claude Desktop**, in `claude_desktop_config.json`. Desktop apps do not
inherit your shell's `PATH`, so give it the absolute path from `which agmem`,
and since there is no project, serve the cross-project `user` space:

```json
{ "mcpServers": { "agmem": {
    "command": "/Users/you/.cargo/bin/agmem",
    "env": { "AGMEM_SPACE": "user" }
} } }
```

No space needs configuring. Each session derives one from where it runs: the
enclosing git project's name, so every worktree of a repo shares a space, else
the directory name. Set `AGMEM_SPACE` only to pin a name the folder does not
already say.

### Inject the briefing on session start

The Claude Code plugin does this through `agmem hook session-start`, which
reads the hook payload on stdin and prints the briefing as hook context — the
same binary answers every plugin hook, so nothing else needs installing. For
any other client, `context` is also a shell command, so a session-start hook
can put the memory block in front of the model before its first token instead
of hoping it asks:

```sh
agmem context --query "release work" --budget-chars 4000
```

It attaches to the running daemon, or starts the one the session is about to
reuse, and prints the same block the MCP tool returns.

## The loop

Seven tools. Two MCP prompts, which Claude Code shows as slash commands, ask
for them at the right moments.

| Tool | Does |
|---|---|
| `remember` | Store distilled claims, optionally with the verbatim episode they came from. An episode given a `title` and `doc_kind` is a document: a plan, review, report, probe or transcript kept whole, versioned by title. Answers with a diff: created, duplicates, superseded, related. |
| `recall` | Ask in words. BM25 and vector search fused, rescored by retention. Every hit carries its signals; a full page says what it cut. |
| `context` | The session-start block: Instructions, Profile, Relevant, Lessons, within a character budget, every line ending in its id. |
| `forget` | Close a memory, or purge it. By query it takes two identical calls, the first a dry run, so a deletion never reaches something that merely resembles the request. A document is not purged while live claims cite it, unless `cascade` takes them with it. |
| `inspect` | The paper trail: a claim's correction chain, the episode behind it, everything ever said about an entity, a document by title read one chunk at a time, the documents a space holds, or per-space counts. |
| `consolidate` | What needs tidying and nothing done about it: near-duplicate groups, contradiction candidates, stale working notes, over-full tags, documents nothing cites. Full text on every candidate. |
| `reflect` | Store a conclusion with the ids it was drawn from. A `summary` stands in for its cited claims when `context` runs short of budget. |

| Prompt | Asks the agent to |
|---|---|
| `/mcp__agmem__recall_first` | Read the memory block before the first move, and correct it rather than work around it |
| `/mcp__agmem__checkpoint` | Review the session, recall each candidate to find what it corrects, then write the batch with `supersedes` on the corrections |

### A round trip

Store a claim. One atomic, self-contained statement per entry, third person:

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

## Spaces

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
| `--embedder` / `AGMEM_EMBEDDER` | `fastembed` | The local ONNX model, the only backend |
| `--pool` / `AGMEM_POOL` | 64 | Candidate pool before rescoring |
| `--max-k` / `AGMEM_MAX_K` | 50 | Ceiling for `recall`'s `k` |
| `--idle-timeout` / `AGMEM_IDLE_TIMEOUT` | 600 | Seconds the daemon outlives its last session |
| `--no-daemon` / `AGMEM_NO_DAEMON` | off | Own the store in this process |
| `--log`, `--log-file` | `info` to stderr | Telemetry. stdout is the MCP wire and stays empty |
| `AGMEM_TOOL_DESC_<TOOL>` | built-in wording | Replace one tool's description, per server, no rebuild |
| `FASTEMBED_CACHE_DIR` | `<data>/models` | Where the embedding model lives |
| `--doctor` | | Self-check, then exit |
| `--reindex` | | Re-embed every row under the configured embedder. The one way to change models |

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
- **The first run stalls on the model download.** It pulls 65 MB from Hugging
  Face into `<data dir>/models`. Behind a proxy, copy that directory from a
  machine that has it, or point `FASTEMBED_CACHE_DIR` at one that does.
- **A different model.** A store written with one model refuses to open under
  a different one; `--reindex` converts it. There is no model-less mode:
  recall is BM25 *and* vectors, and ONNX Runtime is a hard requirement.
- **Starting over.** Delete the data directory. Keep `models/` to skip the
  download.

## Development

```sh
cargo test --workspace                                   # unit, integration and the offline quality eval
cargo clippy --workspace --all-targets -- -D warnings
```

Four crates: `agmem-core` (records, scoring, dedup, chunking; no I/O),
`agmem-store` (SurrealDB schema and queries), `agmem-embed` (ONNX and no-op
backends) and `agmem-server` (the MCP service and the `agmem` binary).
CI never downloads a model: tests that need real semantics replay recorded
BGE vectors (`tests/fixtures/`), and tests that need the live model are
`#[ignore]`d.

The repo's own `.claude/` is the ctx-flow framework — routing discipline,
agents, hooks, commands — tuned to run on the plugin under `plugin/`. Nothing
in the repo enables the plugin; sessions here use it the way any user does,
installed from the marketplace at user scope, so a plugin change is dogfooded
once it is released. `claude --plugin-dir ./plugin` loads the working-tree
plugin without installing it; `claude plugin marketplace add /path/to/agmem`
installs it from a checkout.

Retrieval quality is measured, not asserted. An offline, deterministic eval
rides `cargo test` against a recorded baseline in
[docs/eval/quality.md](docs/eval/quality.md), and an LLM-driven harness in
`scripts/desc-eval.nu` measures whether a tool description gets the tool
called. Both are the gate for changes to ranking or wording.

### Contributing and releasing

`main` is protected and PR-only. CI runs fmt, clippy with warnings denied, and
the full suite on Linux and macOS.

A release is one merge. release-plz keeps a rolling PR open proposing the next
version, and merging it pushes the tag. The tag fires cargo-dist, which builds
every target, publishes the GitHub release with build attestations and the
shell installer, and updates the Homebrew tap. Nothing after the merge is
manual.

## Docs

- [docs/design.md](docs/design.md) — architecture, schema, tool contracts, retrieval and decay
- [docs/tool-descriptions.md](docs/tool-descriptions.md) — what the descriptions say, and the measured effect
- [docs/eval/](docs/eval/) — quality baseline, fusion sweep, rerank and NLI probes
- [docs/idea.md](docs/idea.md) — the research this is built on

## License

MIT or Apache-2.0, at your option.
