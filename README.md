# agmem

Memory for coding agents, over MCP. One local process, one embedded database,
no server-side LLM: the agent distils what is worth keeping, agmem stores it,
dates it, ranks it, and shows its work.

Six tools — `remember`, `recall`, `context`, `forget`, `inspect`,
`consolidate` — and two rituals that ask for them.

> Status: Phase 2 complete, and `consolidate` (Phase 3) has landed. The loop,
> the session-start block, removal, startup pruning, the shared daemon, the
> rituals and candidate surfacing all work end to end from Claude Code. The
> entity graph and the memory-quality eval are what is left of Phase 3.

## Install

Requires Rust 1.89+. The crate is `agmem-server`; the binary it installs is
`agmem`.

```sh
cargo install --git https://github.com/AlfoldiMate/agmem agmem-server
# or, from a clone:  cargo install --path crates/agmem-server
agmem --doctor
```

`--doctor` is the install check: it creates the data directory, looks for a
running daemon, takes the single-writer lock, opens the database, runs
migrations, does a write/read roundtrip, and loads the embedding model. The
first run downloads that model into `<data dir>/models` — BGE-small-en-v1.5,
quantised, 65 MB on disk — which took 23 seconds here on a warm connection.
Every run after that is offline.

```
agmem doctor
  ok    data dir writable    ~/Library/Application Support/dev.agmem.agmem
  ok    tool descriptions    agmem's own wording
  ok    shared daemon        not running; the next session starts one
  ok    single-writer lock   held by this process
  ok    database open        surrealkv://…/agmem.db
  ok    schema               v1
  ok    write/read roundtrip scratch record created and removed
  ok    embedder             bge-small-en-v1.5-q (384d)
  ok    embedder vs store    same model and width
doctor: all checks passed
```

That report goes to **stderr**, with a few INFO log lines; stdout is the MCP
wire and stays empty even here. The exit status is 0 only when every check
passed, so `agmem --doctor` works as a CI or setup gate.

### Where it keeps things

| | |
|---|---|
| macOS | `~/Library/Application Support/dev.agmem.agmem` |
| Linux | `~/.local/share/agmem` (`$XDG_DATA_HOME/agmem`) |
| Windows | `%APPDATA%\agmem\agmem\data` |

One directory holds all of it: `agmem.db/` (the store), `models/` (the
embedding model), `agmem.lock`, and, while sessions are running, `agmem.sock`
and `daemon.log`. Back it up, move it or delete it as a unit. `--data` /
`AGMEM_DATA` points somewhere else — a scratch dir per experiment is the easiest
way to try something without touching real memory.

## Register it

The same three lines in every client: an `mcpServers` entry naming the binary
and this project's space.

**Claude Code** — `.mcp.json` in the project root, checked in so everyone on
the repo gets it:

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

or, without editing a file:

```sh
claude mcp add agmem --scope project -e AGMEM_SPACE=myproject -- agmem
```

This repo's own `.mcp.json` is the working example.

**Cursor** — `.cursor/mcp.json` for one project, `~/.cursor/mcp.json` for all of
them. Identical shape:

```json
{ "mcpServers": { "agmem": {
    "command": "agmem",
    "env": { "AGMEM_SPACE": "myproject" }
} } }
```

**Claude Desktop** — `claude_desktop_config.json`, reachable from Settings →
Developer → Edit Config, at
`~/Library/Application Support/Claude/claude_desktop_config.json` on macOS and
`%APPDATA%\Claude\claude_desktop_config.json` on Windows:

```json
{ "mcpServers": { "agmem": {
    "command": "/Users/you/.cargo/bin/agmem",
    "env": { "AGMEM_SPACE": "user" }
} } }
```

**Give Desktop the absolute path.** A desktop app is not launched from your
shell and does not inherit its `PATH`, so a bare `agmem` fails to spawn with no
other clue; `which agmem` prints what to paste. Desktop also has no project, so
the cross-project `user` space is usually the right one there.

Any other MCP client works the same way: agmem is a stdio server, `command:
"agmem"`, no arguments.

`AGMEM_SPACE` names this project's memory. One space per project, plus the
reserved `user` space for what follows the person everywhere — preferences,
working style, things true regardless of repo.

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

The daemon is a Unix socket, so on Windows — and with `--no-daemon`, or a
remote `--db` — agmem opens the store in the session's own process instead.
That is one session at a time on an embedded store.

### One store, several machines

Point every agmem at a SurrealDB server rather than an embedded file:

```json
{ "mcpServers": { "agmem": {
    "command": "agmem",
    "env": {
      "AGMEM_SPACE": "myproject",
      "AGMEM_DB": "ws://memory.internal:8000"
    }
} } }
```

The server becomes the single-writer boundary, so there is no lock file and no
daemon — `--doctor` reports `skip single-writer lock, remote engine`. agmem
uses the `agmem` namespace and the `main` database and applies its own
migrations on first connect. It sends no credentials, so the server has to
accept the connection unauthenticated: this is a trusted-network arrangement,
not an exposed one.

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

Add `episode` and the conversation those claims came from is stored unedited as
ground truth — chunked for retrieval, never rewritten — with every claim in the
same call provenanced to it:

```json
{ "memories": [ { "content": "The user prefers Rust over Python for command-line tools" } ],
  "episode": { "content": "…the raw turn, quoted rather than summarised…" } }
```

Those chunks compete in `recall` and come back as `kind: "episode"`;
`inspect` shows the text behind any claim that names one.

Send the same claim again and nothing is written — the claim already stored is
reported instead, with **what it says**, how close a match it was, and which
entry of your batch it refers to:

```json
{ "created": [],
  "duplicates": [ { "id": "01M14XWWAXJG…", "of": 0, "similarity": 0.9999998,
                    "content": "The user prefers Rust over Python for CLI tools" } ] }
```

Identical text lands a rounding error short of 1.0; a reworded version of the
same claim lands wherever the near-duplicate gate caught it.

`content` is there because **a correction reads much like the claim it
corrects**, so it arrives here rather than in `created` — measured at 5 of 6
runs. Without the text, an agent sees an id and `0.957` and answers "already
noted", while the claim that is still live is the old and wrong one. With it,
the same scenario supersedes 3 times out of 3. A write that *was* stored gets
the same courtesy under `related`: live claims about the same subject, close
enough to be worth reading, far enough apart not to be duplicates.

```json
{ "created": ["01M14XWWB6PG…"],
  "related": [ { "id": "01M14XWWAXJG…", "of": 0, "similarity": 0.84,
                 "content": "The user formats Python with black." } ] }
```

Neither list is a verdict — agmem never decides that two claims disagree. It
hands back the id and the text; the agent re-sends with `supersedes` if it is a
correction, and ignores it if it is not.

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
surfaced only because it never decays is visible as such.

When the answer fills `k` and more claims match than came back, it also carries
what it left behind:

```json
{ "truncated": { "matching_claims": 312, "returned_claims": 50, "k": 50,
                 "note": "These are the 50 strongest of 312 live claims these filters select — a ranked page…" } }
```

A page of hits and a whole store serialise identically, so without this an
agent asked what memory holds about a subject answers from the top fifty and
has no way to know it. `truncated` is absent when nothing was cut. Reading a
page is still not an audit: what is duplicated or out of date needs every claim
compared against every other, which is `consolidate`.

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
through `remember`'s `supersedes` without a `recall` in between. A section with
nothing to say leaves no heading behind. Verbatim episode text never appears —
one chunk would eat a quarter of the budget, and `recall` is the way to it.
Nothing is reinforced either: the block is read on a schedule, so being in it is
no evidence a memory was useful.

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

**`consolidate`** — what needs tidying up, and nothing done about it. Three
lists: `near_duplicates` (groups of live claims saying the same thing, each
group one `remember(supersedes:)` call away from merged), `contradictions`
(pairs naming one subject, offered so you can decide which is true — the same
pair may also appear under `near_duplicates`, because a cosine carries the
subject and not the polarity) and `stale_contexts` (notes filed `fast` that
recall has kept alive far past the twenty-day horizon their class implies).
Every candidate carries its full text, not just its id — there is no
server-side LLM here, so the decision is the agent's, and an id and a number
are not something to decide on.

```json
{ "spaces": ["myproject"],
  "scanned": [{ "space": "myproject", "compared": 34, "truncated": false }],
  "near_duplicates": [
    { "space": "myproject", "min_similarity": 0.94, "max_similarity": 0.97,
      "members": [
        { "id": "01M14XWWAXJG…", "content": "The user formats Python with black" },
        { "id": "01M14XY6T7RX…", "content": "Python in this repo is formatted by black" } ] } ],
  "contradictions": [],
  "stale_contexts": [
    { "claim": { "id": "01M14Y1ZK9PQ…", "content": "The branch under review is spike",
                 "decay_class": "fast", "access_count": 30 },
      "idle_days": 201.4, "expires_in_days": 418.1 } ] }
```

`min_similarity` is the weakest pair *in* a group, not the weakest link that
formed it: clusters are transitive, so a low number is how a group that chained
together through a middle claim tells you it may not be one claim at all.
Unlike every other read here, `consolidate` looks in the current space alone —
a tidy-up should not reach the shared `user` space unless you name it.

## Two rituals

The six tools are what an agent *may* call. These two are what you ask it to
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
| `FASTEMBED_CACHE_DIR` | `<data>/models` | Where the embedding model is downloaded and read from |
| `--log` / `AGMEM_LOG`, `--log-file` / `AGMEM_LOG_FILE` | agmem at `info`, its dependencies at `warn`, stderr | Telemetry |
| `--no-daemon` / `AGMEM_NO_DAEMON` | off | Own the store in this process; one session at a time |
| `--idle-timeout` / `AGMEM_IDLE_TIMEOUT` | 600 | Seconds the daemon outlives its last session; 0 keeps it |
| `--doctor` | — | Self-check, then exit |

stdout is the MCP wire: all logging goes to stderr or `--log-file`, never
stdout. `agmem --help` is the same list with the exact spellings.

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

The walkthrough this is signed off against — a clean data directory, the two
sessions sharing one daemon. Last run 2026-08-29 against the release binary
over raw JSON-RPC, all passing:

1. `agmem --doctor` on an empty `--data` — every line `ok`, nothing on stdout.
   23s the first time (model download included), 1.9s after.
2. **Session A**: `remember` three claims plus the episode they came from →
   three ids and an episode id.
3. **Session B** (a second process on the same data dir, attached to the daemon
   A started): `recall` a question about them → the claims come back, and the
   episode chunk with them.
4. Re-send one claim verbatim and once reworded → nothing written; both
   reported as duplicates of the stored id, at `0.9999998` and `0.983`.
5. `remember` a correction with `supersedes` → the new id in `created`, the old
   one in `superseded`.
6. `inspect memory:<the old id>` → a two-link chain, the old one dated,
   `invalid_reason: "superseded"` and pointing at its replacement.
7. `inspect stats` → `live: 3, invalidated: 1, episodes: 1, chunks: 1`.
8. `context` → Instructions, Relevant and Lessons, each line ending in its id;
   no Profile heading, because the only `identity` fact was the one just
   superseded.
9. `forget` by query without a dry run → refused, naming the two-step. The same
   call with `dry_run: true` → one match; sent again unchanged → invalidated.
10. `prompts/list` → `recall_first` and `checkpoint`.
11. `consolidate`, on a store seeded into all three states it reports — one
    deploy fact written three ways in a single call (the gate never compares
    two entries of one batch, so all three go live), two test-runner claims
    that disagree, and a `fast` note backdated 40 days against its 20-day
    horizon with the `surreal` CLI, because nothing in the tool surface can
    write that state. Answer: one five-member cluster, `min_similarity` 0.878
    against `max_similarity` 0.95 — it chained through the shared subject, and
    says so; 15 contradiction candidates, every pair of the six claims naming
    `atlas`, with the genuine disagreement second at 0.948 behind a pair of
    duplicates at 0.950; and one stale context, idle 40.0 days with
    `expires_in_days` 19.9 at `access_count` 8. Ranking that list by cosine
    puts duplicates above disagreements, which is the open half of this — see
    the ledger.

## Troubleshooting

- **`another agmem process (pid N) already owns the data dir …`** — that pid
  is usually the shared daemon, and the usual cause is `--no-daemon` on a
  session that could have attached to it. Drop the flag, stop that process, or
  point both at one SurrealDB server with `AGMEM_DB=ws://…`.
- **A session came up with no memory tools** — read `<data dir>/daemon.log`:
  the shared store failed to start, and the session refused rather than open a
  second copy of a single-writer store.
- **Claude Desktop shows the server as failed** — almost always the `PATH`: it
  needs the absolute path to the binary, not `agmem`. Its own log names the
  spawn error.
- **`--doctor` says `skip` on three lines** — that is a healthy report with a
  daemon running. The lock, the database and the schema belong to the daemon,
  and checking them from here would open the second copy of the store the whole
  design exists to prevent. Stop the sessions (or add `--no-daemon`) for the
  full report.
- **The first run hangs or fails on the model download** — it is pulling ~65 MB
  from Hugging Face into `<data dir>/models`. Behind a proxy or an air gap,
  copy that directory from a machine that has it, or set
  `FASTEMBED_CACHE_DIR` to wherever it already lives — that variable *replaces*
  the data-dir default rather than adding to it, so one cache can serve several
  data dirs. A half-finished download is safe to delete; the directory is a
  cache, and `agmem --doctor` refills it.
- **First call after a start is slow** — the model loads on start;
  `agmem --doctor` once after install gets the download out of the way, and the
  shared daemon means later sessions attach to an already-loaded one.
- **A `recall` came back without something you know is stored** — two faults
  behind this, both fixed. [#39](https://github.com/AlfoldiMate/agmem/issues/39)
  emptied the fulltext arm whenever the question contained a word the stored
  claim did not, and [#40](https://github.com/AlfoldiMate/agmem/issues/40) had
  the vector arm come back short on every recall a process served, because a
  filter travelling inside the vector scan loses rows on a cold index. If it
  still happens, drop `query` and filter on `entities`/`tags`/`kinds`;
  `inspect stats` says whether the row is there at all.
- **No ONNX Runtime on the platform** — `--embedder none` runs BM25-only, and
  `cargo install --no-default-features` drops the ONNX build entirely. A store
  written with one model refuses to open under a *different* one: the model and
  width are recorded, and `--doctor` reports the mismatch. Dropping to `none` is
  always allowed — it stores no vectors.
- **Starting over** — delete the data directory. Everything agmem wrote is
  under it, and the next start recreates the schema. Keep `models/` if you do
  not want the download again.

## Docs

- `docs/design.md` — architecture, schema, tool contracts, flows
- `docs/tool-descriptions.md` — what the tool descriptions say, measured
- `docs/idea.md` — the research this is built on

License: MIT OR Apache-2.0
