# agmem

Memory for coding agents, over MCP. One local process, one embedded database,
no server-side LLM: the agent distils what is worth keeping, agmem stores it,
dates it, ranks it, and shows its work.

Five tools — `remember`, `recall`, `context`, `forget`, `inspect`.

> Status: Phase 1 (MVP) plus the `context` block and `forget`. The loop works
> end to end from Claude Code. Startup pruning, prompts and consolidation are
> Phase 2+.

## Install

Requires Rust 1.89+.

```sh
cargo install --git https://github.com/AlfoldiMate/agmem agmem-server
# or, from a clone:  cargo install --path crates/agmem-server
agmem --doctor
```

`--doctor` is the install check: it creates the data directory, takes the
single-writer lock, opens the database, runs migrations, loads the embedding
model, and does a write/read roundtrip. The first run downloads the model
(~30 MB, BGE-small-en-v1.5 quantised) into `<data dir>/models` — about 25
seconds on a warm connection; every run after that is offline.

```
agmem doctor
  ok    data dir writable    ~/Library/Application Support/dev.agmem.agmem
  ok    single-writer lock   held by this process
  ok    database open        surrealkv://…/agmem.db
  ok    schema               v1
  ok    write/read roundtrip scratch record created and removed
  ok    embedder             bge-small-en-v1.5-q (384d)
  ok    embedder vs store    same model and width
doctor: all checks passed
```

## Register it

`.mcp.json` in the project root — that is the whole install story:

```json
{
  "mcpServers": {
    "agmem": {
      "command": "agmem",
      "env": {
        "AGMEM_SPACE": "myproject"
      }
    }
  }
}
```

`AGMEM_SPACE` names this project's memory. One space per project, plus the
reserved `user` space for what follows the person everywhere — preferences,
working style, things true regardless of repo. This repo's own `.mcp.json` is the working example.

**Several sessions at once, one store.** The embedded store is single-writer,
so the first session that needs it starts a small background daemon to own it
and every session after that attaches to the same one. Nothing to install and
nothing to start: the first `agmem` does it, and the daemon shuts itself down
ten minutes after the last session detaches.

That is what keeps one memory across projects — a second window, another repo,
a `--resume` alongside a running session all read and write the same store, so
`space: "all"` and the shared `user` space mean what they say.
`ls <data dir>/agmem.sock` says whether one is up, `agmem --doctor` reports it,
and its log is `<data dir>/daemon.log`. `--no-daemon` goes back to one process
owning the store; `AGMEM_IDLE_TIMEOUT=0` keeps the daemon until the machine
restarts.

## The loop

**`remember`** — store distilled claims, optionally with the verbatim text they
came from. One atomic, self-contained statement per entry, third person:

```json
{
  "memories": [
    { "content": "The user prefers Rust over Python for command-line tools",
      "kind": "fact", "entities": ["user"], "tags": ["identity"] },
    { "content": "cargo test -p agmem-store needs a mem:// URL; surrealkv locks the data dir",
      "kind": "lesson" }
  ]
}
```

It answers with a **diff**, not an acknowledgement:

```json
{ "created": ["01M14XWWAXJG…", "01M14XWWB6PG…"],
  "duplicates": [], "superseded": [], "episode": "01M14XWWANHH…" }
```

Send the same claim again and nothing is written — the claim already stored is
reported instead, with how close a match it was (`1.0` once case and whitespace
are folded, otherwise the cosine similarity that tripped the near-duplicate
gate) and which entry of your batch it refers to:

```json
{ "created": [],
  "duplicates": [ { "id": "01M14XWWAXJG…", "of": 0, "similarity": 1.0 },
                  { "id": "01M14XWWAXJG…", "of": 1, "similarity": 0.967 } ] }
```

**`recall`** — ask in words. Both halves of retrieval use the question: BM25
matches the wording, vectors match the meaning, and the two are fused, then
rescored by how well each claim has held up since it was last used.

```json
{ "query": "what language does the user want for command-line tools?", "k": 5 }
```

```json
{ "spaces": ["myproject", "user"],
  "hits": [ { "id": "01M14XWWAXJG…", "kind": "fact",
              "content": "The user prefers Rust over Python for command-line tools",
              "space": "myproject", "score": 0.925,
              "signals": { "rrf": 0.0164, "rrf_normalized": 1.0,
                           "retention": 1.0, "importance": 0.5 },
              "source": "episode:01M14XWWANHH…",
              "entities": ["user"], "tags": ["identity"],
              "valid_from": "2026-08-28T19:35:09.909156Z" } ] }
```

Every hit carries the `signals` behind its place in the order, so a claim that
surfaced only because it never decays is visible as such. Episode chunks
compete in the same ranking and come back as `kind: "episode"`.

**Corrections** — never store a contradiction; send `supersedes`:

```json
{ "memories": [
  { "content": "The user prefers Go over Python for command-line tools",
    "supersedes": "01M14XWWAXJG…" }
] }
```

The old claim stays readable and dated; only the new one is live.

**`context`** — the session-start block. Four fixed sections in a fixed order,
capped at `budget_chars`, dropping whole entries rather than cutting one in
half. Read it before your first move; pass `query` to aim the Relevant section
at the work in front of you.

```json
{ "query": "what am I doing in the API gateway?", "budget_chars": 6000 }
```

```markdown
# Memory context (spaces: myproject + user)

## Instructions
- Never force-push to main `01M14XWWAXJG…`

## Profile
- The user prefers Rust over Python for command-line tools `01M14XY6T7RX…`

## Relevant
- The API gateway is deployed from the infra repo `01M14XZ2QK8M…`

## Lessons
- The build breaks on a cold cargo cache `01M14Y0PB3WD…`
```

Every line carries its id, so a claim you find is wrong goes straight back
through `remember`'s `supersedes` without a `recall` in between. Verbatim
episode text never appears — one chunk would eat a quarter of the budget, and
`recall` is the way to it. Nothing is reinforced either: the block is read on a
schedule, so being in it is no evidence a memory was useful.

**`forget`** — removal, with the scope confirmed before anything moves. By
default it *closes* a memory rather than deleting it: it stops answering
`recall` and `context`, and stays readable through `inspect`, dated and marked
`forgotten`. Reach for `remember`'s `supersedes` first — a claim that turned out
to be wrong is a correction, not a mistake to erase.

```json
{ "ids": ["01M14XWWAXJG…"] }
{ "spaces": ["myproject", "user"], "dry_run": false, "purge": false,
  "matched": [ { "id": "01M14XWWAXJG…", "kind": "memory",
                 "content": "The API gateway is deployed from the infra repo",
                 "space": "myproject" } ],
  "invalidated": ["01M14XWWAXJG…"], "purged": [], "chunks_purged": 0 }
```

Three rules worth knowing before you call it:

- **By query, it takes two calls.** Send it once with `dry_run: true`, read
  `matched`, then send the identical call again to act. Anything else — a
  different query, the same query with `purge` flipped, a second execution — is
  refused. A query matches on the *words* you write, not on their meaning:
  BM25 only, deliberately, because a deletion must never reach something that
  merely resembles what you asked for.
- **`purge: true` deletes, and takes the correction chain with it.** That is
  unrecoverable, and it is the only way to remove text that must not stay on
  disk. The dry run lists every row it will take, chain included.
- **Verbatim text can only be purged, never closed** — an episode has no
  validity window — and purging it leaves the claims distilled from it
  standing, still naming where they came from.

**Working context expires on its own.** A memory written with
`decay_class: "fast"` — the class for what is only true this week — is closed
at the next server start once about twenty days have passed without a recall,
marked `expired`. Every recall pushes that horizon out. Nothing else expires:
a `fact` written a year ago is still live, just ranked lower.

**`inspect`** — the paper trail. `ref` takes a memory id (the correction chain,
oldest first, plus the verbatim text behind it), `episode:<id>`,
`entity:<name>` (everything ever said about a subject, corrected claims
included), or `stats` for per-space counts.

```json
{ "ref": "memory:01M14XWWAXJG…", "spaces": ["myproject", "user"],
  "found": { "kind": "memory",
    "chain": [
      { "id": "01M14XWWAXJG…", "content": "The user prefers Rust over Python…",
        "invalid_at": "2026-08-28T19:35:53.415031Z",
        "invalid_reason": "superseded", "superseded_by": "01M14XY6T7RX…" },
      { "id": "01M14XY6T7RX…", "content": "The user prefers Go over Python…" } ] } }
```

## Two rituals

The five tools are what an agent *may* call. These two are what you ask it to
do — MCP prompts, which Claude Code shows as slash commands:

| | |
|---|---|
| `/mcp__agmem__recall_first` | Read the memory block before the first move, work from it, and correct it rather than working around it |
| `/mcp__agmem__checkpoint` | Review the session, recall each candidate to find what it corrects, then write the batch with `supersedes` on the corrections |

Both take an optional focus — `/mcp__agmem__checkpoint the auth refactor` —
which narrows what the ritual looks at. Neither touches the store itself: what
comes back is an instruction, and the agent's next turn is what runs.

They exist because a tool description and a ritual are read at different
moments. A description is one option among several while the model is deciding
what to do next; measured against Claude Code, whose own memory is named in the
system prompt, `remember` was reached for in 0 of 6 sessions that all replied
"Saved". Add `/mcp__agmem__checkpoint` to the identical sessions and it is 6 of
6, each one recalling first and then writing a batch. A ritual is not in that
competition — you asked for it, so it is the instruction in front of the model.
The numbers and the harness behind them are in `docs/tool-descriptions.md`.

## Spaces

| `space` value | means |
|---|---|
| omitted | write: this server's space. read: this space **and** `user` |
| `current` | this server's space |
| `user` | the cross-project space; writes there must say so explicitly |
| `all` | every registered space (read only) |
| a name | that space |

## Configuration

| Flag / env | Default | Meaning |
|---|---|---|
| `--data` / `AGMEM_DATA` | platform data dir | Database, lock file, model cache |
| `--db` / `AGMEM_DB` | `surrealkv://<data>/agmem.db` | Engine; `mem://` for scratch, `ws://host` to share one store |
| `--space` / `AGMEM_SPACE` | `default` | This instance's space |
| `--embedder` / `AGMEM_EMBEDDER` | `fastembed` | `fastembed`, or `none` for BM25-only |
| `--pool` / `AGMEM_POOL` | 64 | Candidate pool before rescoring |
| `--max-k` / `AGMEM_MAX_K` | 50 | Ceiling for `recall`'s `k` |
| `AGMEM_TOOL_DESC_<TOOL>` | agmem's own wording | Replace one tool's description — see below |
| `--log` / `AGMEM_LOG`, `--log-file` / `AGMEM_LOG_FILE` | agmem at `info`, its dependencies at `warn`, stderr | Telemetry |
| `--no-daemon` / `AGMEM_NO_DAEMON` | off | Own the store in this process; one session at a time |
| `--idle-timeout` / `AGMEM_IDLE_TIMEOUT` | 600 | Seconds the daemon outlives its last session; 0 keeps it |
| `--doctor` | — | Self-check, then exit |

stdout is the MCP wire: all logging goes to stderr or `--log-file`, never
stdout.

### Rewording a tool

A tool description is most of what decides whether an agent reaches for memory,
and what counts as good wording depends on the model, the client and the work.
`AGMEM_TOOL_DESC_<TOOL>` replaces one outright — `REMEMBER`, `RECALL`,
`CONTEXT`, `FORGET`, `INSPECT`, per server, no rebuild:

```jsonc
{ "mcpServers": { "agmem": {
    "command": "agmem",
    "env": {
      "AGMEM_SPACE": "myproject",
      "AGMEM_TOOL_DESC_RECALL": "Search what earlier sessions stored. Call it before answering anything about this project's history."
    }
} } }
```

The override is the whole description, not an addition, so what the agent reads
is exactly what you wrote. A variable naming something that is not one of the
five tools stops the server with a message rather than being ignored — a typo
here is invisible otherwise. `agmem --doctor` and the startup log both report
which tools a run is rewording, and each project keeps its own wording even
when several share one daemon.

`docs/tool-descriptions.md` has the measured effect of the built-in wording, and
the harness (`scripts/desc-eval.nu`) that measures it, if you want to check
your own.

## Verify the loop

Five checks, in two separate sessions — this is the dogfood checklist the MVP
is signed off against (run 2026-08-28 against a fresh data dir, all passing):

1. `agmem --doctor` on an empty data directory — every line `ok`, nothing on
   stdout.
2. **Session A**: `remember` three claims plus the verbatim episode they came
   from → three ids and an episode id.
3. **Session B** (a new process, same data dir): `recall` a question about them
   → the claims come back, and the episode chunk with them.
4. Re-send one claim verbatim and once reworded → nothing written; both
   reported as duplicates of the stored id, at `1.0` and `0.967`.
5. `remember` a correction with `supersedes` → the old claim disappears from
   `recall`, and `inspect` shows the two-link chain with the old one dated and
   marked `superseded`. `inspect entity:user` shows both; `inspect stats`
   counts `live: 3, invalidated: 1, episodes: 1`.

## Troubleshooting

- **`another agmem process (pid N) already owns the data dir …`** — that pid
  is usually the shared daemon, and the usual cause is `--no-daemon` on a
  session that could have attached to it. Drop the flag, stop that process, or
  point both at one SurrealDB server with `AGMEM_DB=ws://…`.
- **A session came up with no memory tools** — read `<data dir>/daemon.log`:
  the shared store failed to start, and the session refused rather than open a
  second copy of a single-writer store.
- **A `recall` came back without something you know is stored** — on a small
  store, a query-shaped `recall` can leave out a live memory that a
  filters-only `recall` (drop `query`, keep `entities`/`tags`/`kinds`) returns.
  Known bug, [#39](https://github.com/AlfoldiMate/agmem/issues/39); `inspect
  stats` tells you whether the row is there at all, and dropping the query is
  the workaround until it is fixed.
- **First call is slow** — the model loads on start; `--doctor` once after
  install gets the download out of the way.
- **No ONNX Runtime on the platform** — `--embedder none` runs BM25-only, and
  `cargo install --no-default-features` drops the ONNX build entirely.

## Docs

- `docs/design.md` — architecture, schema, tool contracts, flows
- `docs/tool-descriptions.md` — what the tool descriptions say, measured
- `docs/idea.md` — the research this is built on

License: MIT OR Apache-2.0
