# agmem

Memory for coding agents, over MCP. One local process, one embedded database,
no server-side LLM: the agent distils what is worth keeping, agmem stores it,
dates it, ranks it, and shows its work.

Three tools — `remember`, `recall`, `inspect`.

> Status: Phase 1 (MVP). The loop works end to end from Claude Code.
> `forget`, `context` assembly and consolidation are Phase 2+.

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

**One session at a time, for now.** The embedded store is single-writer and
the data directory is shared by every project, so the second concurrent Claude
Code session starts a second `agmem`, fails to take the lock, and comes up
without memory tools ([#37](https://github.com/AlfoldiMate/agmem/issues/37)).
Until that lands, either keep one agmem-enabled session open, or give a project
its own store with `"AGMEM_DATA": "/path/to/its/own/dir"` — at the cost of the
shared `user` space.

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
| `--log` / `AGMEM_LOG`, `--log-file` / `AGMEM_LOG_FILE` | agmem at `info`, its dependencies at `warn`, stderr | Telemetry |
| `--doctor` | — | Self-check, then exit |

stdout is the MCP wire: all logging goes to stderr or `--log-file`, never
stdout.

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

## Known limitations

- Only one session at a time can hold the store
  ([#37](https://github.com/AlfoldiMate/agmem/issues/37)) — see above.

## Troubleshooting

- **`another agmem process (pid N) already owns the data dir …`** — the
  embedded store is single-writer. Stop the other instance, or point both at
  one shared SurrealDB server with `AGMEM_DB=ws://…`.
- **First call is slow** — the model loads on start; `--doctor` once after
  install gets the download out of the way.
- **No ONNX Runtime on the platform** — `--embedder none` runs BM25-only, and
  `cargo install --no-default-features` drops the ONNX build entirely.

## Docs

- `docs/design.md` — architecture, schema, tool contracts, flows
- `docs/idea.md` — the research this is built on

License: MIT OR Apache-2.0
