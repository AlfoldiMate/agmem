# Plan — #134 documents: write fields, windowed inspect, title chain, cascade purge

Schema v9 shipped SurrealQL only: **no Rust file mentions `title` or `doc_kind`**
(`rg title crates/*/src` is empty). #134 is the full end-to-end plumb.

## Decisions

1. **Title supersession — no schema v10.** "Newest by title wins" via
   `ep_title (space, title)` + `ORDER BY created_at DESC`. `episode` has no
   UPDATE path at all (`queries/write.rs:34-53` only CREATEs), so a
   `superseded_by` link would mean a second statement into an append-only table
   *and* a chain walk in `forget` (the "purge takes the whole chain" rule,
   design §5.4). Reads that want "current" already filter `space + title`, so
   ordering is free on the index. `inspect` on a document returns
   `versions: [{id, created_at, content_hash}]` newest→oldest — the chain is
   visible without a column. **v10 not required.**
2. **`derived_from` spans — defer.** The `spans` sidecar exists (v9) but writing
   it needs a span-carrying input shape on `remember`/`reflect`, char-offset
   validation against verbatim content, and a reader. Nothing reads it; not
   trivial. Out of #134.
3. **Soft forget on a document stays refused** — `refuse_closing_text`
   (`forget.rs:522`) is unchanged; a document has no validity window either.
   Only the *purge* path gains the citation guard and `cascade`.
4. **Occupancy needs no change.** `recall.rs:549` keys a chunk hit
   `episode:<id>` and a claim distilled from it carries `source =
   episode:<id>` (`recall.rs:729`) — every chunk of a document plus its derived
   claims already share one quota key. Confirmed, no code change.

## Build order

1. `crates/agmem-core/src/model.rs` — `Episode` gains `title: Option<String>`,
   `doc_kind: Option<DocKind>`, `tags: Vec<String>`, `mime: Option<String>`;
   new `DocKind` enum (FromStr/Display, the six schema values) beside `Kind`.
2. `crates/agmem-store/src/types.rs` (`EpisodeRow` ~l.167, `EpisodeReadRow`
   ~l.402, `into_episode`) + `queries/read.rs:124 EPISODE_FIELDS` — carry the
   four columns both ways. `queries/write.rs` needs no change: `$ep_row` is a
   whole-object CONTENT.
3. `crates/agmem-store/src/repo/write.rs:97 NewEpisode` + `:313` construction —
   pass the fields through. Dedupe stays `(space, content_hash)`; a re-put of
   the same normalized text returns `Written::Duplicate` unchanged
   (`writes.rs:259` already asserts this shape).
4. `crates/agmem-server/src/tools/remember.rs` — `EpisodeInput` (l.94) gains the
   four params; `validated()` (l.462) enforces: title required with `doc_kind`,
   title trimmed/non-empty/≤ ~200 chars, `doc_kind` parsed with the valid list
   in the error, tags bounded. `transcript` in `user` needs the resolved space,
   so refuse in `run` after the space resolves, not in `validated`.
5. Store reads — `queries/read.rs`: extend `episode()` (l.329) `derived` clause
   to `(source.ref = $target OR derived_from CONTAINS $target)`; add
   `documents_by_title()`, `documents()` (list), `document_citers()`,
   `orphan_documents()`. `repo/read.rs` (beside `episode()` l.630): `versions`,
   `documents`, `citers`, `orphan_documents`; a `DocumentSummary` type.
6. `crates/agmem-server/src/tools/inspect.rs` — `Reference::Doc(space,title)`
   and `Reference::Docs(space)` in `parse()` (l.~700) for `doc:<space>/<title>`
   and `docs:<space>`; `EpisodeView` gains title/doc_kind/tags/mime; new
   `window: { offset, limit, total, truncated }` and `InspectParams.offset/limit`
   (chars, default = first chunk's length); new `Inspected::Documents` variant.
   `versions` on the Episode arm.
7. `crates/agmem-server/src/tools/recall.rs` — after the occupancy/hop cut, one
   `repo::documents_by_ids` over the distinct episode ids of `HitKind::Episode`
   rows; `Hit` gains `doc: Option<DocRef { id, title, doc_kind, position }>`
   (position = the chunk's `position`, already on the row).
8. `crates/agmem-server/src/tools/forget.rs` + `consolidate.rs` + docs — purge
   of a document loads citers first, refuses without `cascade` naming the count
   and the ids, and with `cascade` appends them to `Forget.memories` (the chain
   expansion at l.243 then applies to each); `ConsolidateResult.orphan_documents`;
   `docs/design.md` + `docs/tool-descriptions.md` deltas; regenerate
   `tests/snapshots/protocol__list_tools.snap`.

## Queries

Citation guard (one space, live only):

```surql
LET $citers = (SELECT record::id(id) AS id, content FROM memory
  WHERE space = $space AND invalid_at IS NONE
    AND (source.ref = $target OR derived_from CONTAINS $target));
```

Title lookup — newest first, `[0]` is current:

```surql
SELECT <EPISODE_FIELDS> FROM episode
 WHERE space = $space AND title = $title ORDER BY created_at DESC
```

Docs list + derived counts (one round trip; kind/tag clauses **built in Rust**,
never bound as possibly-empty lists — `CONTAINSANY []` matches nothing, see the
note at `queries/read.rs` around l.276):

```surql
LET $docs = (SELECT <EPISODE_FIELDS> FROM episode
  WHERE space = $space AND doc_kind IS NOT NONE {kind_clause} {tag_clause}
  ORDER BY created_at DESC LIMIT {limit});
LET $ids = $docs.map(|$d| type::thing('episode', $d.id));
RETURN { docs: $docs, citers: (SELECT source.ref AS src, derived_from FROM memory
  WHERE space = $space AND invalid_at IS NONE
    AND (source.ref IN $ids OR derived_from CONTAINSANY $ids)) };
```

Count per document in Rust from `citers` (a memory citing through both columns
counts once). Orphans for `consolidate` — one pass, not one query per doc:

```surql
LET $cited = (SELECT VALUE source.ref FROM memory
  WHERE space = $space AND invalid_at IS NONE AND source.kind = 'episode');
LET $drv = array::flatten((SELECT VALUE derived_from FROM memory
  WHERE space = $space AND invalid_at IS NONE));
RETURN SELECT <EPISODE_FIELDS> FROM episode
  WHERE space = $space AND doc_kind IS NOT NONE
    AND id NOT IN array::union($cited ?? [], $drv ?? []) ORDER BY created_at DESC;
```

## Risks

- `derived_from CONTAINS` has no index (only `mem_entities`/`mem_tags` carry
  `.*`) → full in-space scan per purge and per consolidate pass. Acceptable at
  current sizes; if it ever needs one it must be `COLUMNS derived_from.*` and
  that *is* a v10 — an index without `.*` silently serves the wrong plan
  (design §2.2 note, verified on 3.2.4).
- `CONTAINSANY []` matches nothing: empty filters must be omitted from the query
  text, the way `Lookup` clauses already are.
- Chunk-vs-episode ids: `unqualified` (inspect.rs) resolves memory → chunk →
  episode; keep that order and make a chunk id still answer as its episode, now
  with the window applied from offset 0. `doc:<space>/…` must refuse a space
  outside the resolved `spaces` — an id/name is a capability inside a space.
- Windows are **char** offsets (matching the `spans` convention and the 100k
  cap); slice with `char_indices`, never byte ranges, or a multibyte document
  panics.
- MCP schema: schemars marks a bare `Vec` *required* whatever serde omits — new
  optional fields must be `Option<T>` + `skip_serializing_if` (see the comment
  on `MemoryView::derived_from`). `protocol__list_tools.snap` (93.7K) will diff;
  and any change to tool *wording* runs `scripts/desc-eval.nu` first (standing
  instruction).
- `remember`'s episode arm gaining params changes the tool schema agents read;
  keep the description delta minimal so the desc-eval cost stays at the
  `--isolated` run.

## Tests

- `crates/agmem-store/tests/writes.rs` — title/doc_kind/tags/mime round-trip;
  same normalized text re-put returns `Written::Duplicate` with the fields of
  the *first* row (extends the pattern at l.259).
- `crates/agmem-store/tests/reads.rs` — episode detail includes a
  `derived_from`-only citer (l.690 covers `source.ref` today); title versions
  ordering; docs list + counts; orphan list.
- `crates/agmem-server/tests/protocol.rs` — the acceptance six. Harness pattern:
  `Harness::start(Arc::new(RecordedEmbedder))` (fixtures at
  `tests/fixtures/protocol/vectors.json`; an unrecorded text **panics** unless
  `AGMEM_RECORD_VECTORS=1`, which needs the live model). Dedupe-by-hash, title
  chain, windowed inspect and cascade refusal/purge do no ranking → use
  `Harness::start(Arc::new(NoopEmbedder))` (as l.789 and l.1500 already do) and
  no fixture grows. Chunk hits carrying `doc` and the occupancy cap over a
  document **do** rank → either reuse episode texts already in the recording or
  budget one `AGMEM_RECORD_VECTORS=1` pass.
- No `migrate.rs`/`schema.rs` test changes — nothing new in the schema.

## Unknowns

- `docs/design.md:371` attributes "title required with doc_kind, transcript
  refused in user" to **#135**, while this task assigns it to #134. Steps 1-4
  are that work; if #135 is a separate PR they will collide in `remember.rs`.
- `docs:<space>` has no paging in the spec — a `limit` (default ~50, newest
  first) is assumed here.
