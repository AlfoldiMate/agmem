# Upstream report draft — `KnnScan` under-returns on a cold index

Ready to file against [surrealdb/surrealdb]. Written at agmem issue #40; the
mitigation agmem ships is in `crates/agmem-store/src/queries/read.rs`
(`OVER_FETCH`, `NEAR_DUP_PROBE`), so nothing here blocks on upstream.

Nothing below is inferred: every number was measured on this machine, and the
one thing the EXPLAIN does *not* show is called out as such.

## Status — 2026-08-29: re-verified, and filed as a comment on #7423

**Filed**: https://github.com/surrealdb/surrealdb/issues/7423#issuecomment-5463333261

The live upstream issue is **surrealdb/surrealdb#7423** (open, 2026-07-17):
same symptom, embedded surrealkv, 3.2.x. It holds delete/overwrite churn —
element/doc id recycling plus compaction — to be the precondition, and reports
**0 strong drops** for the write-only, close-reopen, filtered-query-first
protocol. Our case is that protocol, and it drops — which is why this went
there as a comment rather than as a new issue.

Re-measured today against a **copy** of the repro store
(`~/.local/share/agmem-repro/recall-omits-live-row/`), through the `surreal`
CLI 3.2.3 — a different process from the one that wrote it — with the three
real BGE query vectors committed in
`crates/agmem-server/tests/fixtures/knn_underreturn.json`. All three read the
same, and the store holds **2 memories, 0 invalidated, 1 episode, 1 chunk**:
nothing in it was ever deleted, superseded or overwritten.

| arm | rows (all 3 queries) |
|---|---|
| `space IN $spaces AND invalid_at IS NONE AND` + KNN | 1 |
| `1 = 1 AND` + KNN | 1 |
| `invalid_at IS NONE AND` + KNN | 1 |
| KNN alone | **2** |
| filtered again, same connection | **2** |
| subquery mitigation | **2** |

**The rebuilt-store path still does not reproduce it**, and that is the sharpest
clue in here rather than a caveat: `knn_probe.rs`'s
`a_cold_filtered_arm_finds_both_rows` writes those same two rows with those same
vectors into a fresh store and passes **6 runs out of 6**. Whatever the fault
needs, it is in what the engine left on disk, not in the rows.

---

## Title

KNN search returns fewer rows when a `WHERE` conjunct accompanies the operator,
until an unfiltered KNN runs on the same connection

## Versions

- `surrealdb` crate **3.2.4** (Rust client, embedded)
- `surreal` CLI **3.2.3+20260721.40522d1** — same behaviour
- Engine: **surrealkv**, embedded, single process
- macOS 27.0 (Darwin), aarch64

## Summary

A `SELECT` using the KNN operator returns fewer rows when its `WHERE` carries
any additional conjunct than the same `SELECT` without one — including a
conjunct that excludes nothing, such as `1 = 1`.

The state is **per connection**, and only an *unfiltered* KNN clears it:

- filtered first, on a fresh connection → short
- filtered again → still short
- unfiltered → complete
- filtered again, same connection → now complete, and stays that way

It reproduces on a datastore read by a **connection that did not write it**. A
store seeded and queried on the same connection does not show it, which is
presumably why it is easy to miss: the index built on that connection is
already in whatever state the unfiltered scan otherwise produces.

## Reproduction

Schema (the relevant part):

```surql
DEFINE FIELD embedding ON memory TYPE option<array<float>>;
DEFINE INDEX mem_vec ON memory FIELDS embedding HNSW DIMENSION 384 DIST COSINE;
```

(That is agmem's shipped definition, verbatim from
`crates/agmem-store/src/migrations/v1_schema.surql` — an earlier draft of this
file quoted a `array<float, 384>` / `TYPE F32` variant that has never been in
the tree. Corrected 2026-08-29 before filing.)

The committed probe is `crates/agmem-server/tests/knn_probe.rs`
(`a_cold_filtered_arm_finds_both_rows`, `--ignored`); it fails while the fault
is present, and its failure list is the measurement.

1. Write two rows into `space = 'probe'`, each with a 384-float embedding, on a
   connection that is **closed** afterwards. No deletes, no overwrites.
2. On a **fresh** connection to the same datastore, run these in order, with
   `$vector` a 384-float probe:

```surql
LET $spaces = ['eval'];

-- 1 row
SELECT VALUE record::id(id) FROM memory
  WHERE space IN $spaces AND invalid_at IS NONE AND embedding <|64,80|> $vector;

-- 1 row — the conjunct excludes nothing
SELECT VALUE record::id(id) FROM memory
  WHERE 1 = 1 AND embedding <|64,80|> $vector;

-- 2 rows
SELECT VALUE record::id(id) FROM memory
  WHERE embedding <|64,80|> $vector;

-- 2 rows, from here on
SELECT VALUE record::id(id) FROM memory
  WHERE space IN $spaces AND invalid_at IS NONE AND embedding <|64,80|> $vector;
```

Both rows are live, both are in `eval`, and `k` is 64 against a table of 2.

Observed over three different probe vectors, on both the Rust client and the
CLI, on a fresh process each time:

| query shape | rows |
|---|---|
| `space IN $spaces AND invalid_at IS NONE AND` + KNN | 1 |
| `1 = 1 AND` + KNN | 1 |
| `invalid_at IS NONE AND` + KNN | 1 |
| `space IN $spaces AND` + KNN | 1 |
| KNN alone | **2** |
| KNN in a subquery, conjuncts applied outside | **2** |

The probe vector matters: a stored row's own embedding, or a random vector,
returns both rows. The vectors above are real sentence-embedding output
(BGE-small-en-v1.5, 384d, cosine).

## Plan

`EXPLAIN FULL` shows the predicate pushed into the `KnnScan` node, and also
retained as a `Filter` above it:

```json
{"operator": "ProjectValue",
 "children": [
   {"operator": "Filter",
    "attributes": {"predicate": "space INSIDE ['eval'] AND invalid_at = NONE"},
    "children": [
      {"operator": "KnnScan",
       "attributes": {"dimension": "384", "ef": "80", "index": "mem_vec",
                      "k": "64",
                      "predicate": "space INSIDE ['eval'] AND invalid_at = NONE"}}]}]}
```

**Caveat, so it is not over-read**: that plan was captured on a connection that
had already run an unfiltered scan, so its `output_rows` read 2 — it shows
*where the predicate goes*, not the under-return itself.

Guess at the mechanism, offered only as a starting point: the pushed-down
predicate makes the traversal treat `k` as a budget over nodes it visits and
rejects rather than over rows it emits, and whatever the unfiltered scan
populates on first use removes the early termination.

## Workaround

Run the KNN bare in a subquery and filter its result:

```surql
SELECT VALUE record::id(id) FROM
    (SELECT id, space, invalid_at FROM memory
     WHERE embedding <|256,80|> $vector)
  WHERE space IN $spaces AND invalid_at IS NONE
  LIMIT 64;
```

`k` is over-fetched because the scan no longer knows what the caller wants, so
candidates land on rows the outer filter discards. On a 384-row store across
two spaces this returns a full 64 of 64 where a bare `k = 64` gave 48 — and it
is **faster** than the pushed-down form it replaces, ~72 ms against ~112 ms.

[surrealdb/surrealdb]: https://github.com/surrealdb/surrealdb/issues
