# Making the ONNX embedder required (plan)

## The constraint nobody wrote down

`--embedder none` is not only a user-facing mode — it is the whole test
suite's *offline lever*. CI sets `FASTEMBED_CACHE_DIR: /nonexistent`
(.github/workflows/ci.yml:12) so that no test can load a model, and seven
child-process test files pass `--embedder none` for exactly that reason:

- crates/agmem-server/tests/doctor.rs:13,29,47,69 (+ the file doc at :3)
- crates/agmem-server/tests/stdio.rs:23,138
- crates/agmem-server/tests/ws.rs:101,242
- crates/agmem-server/tests/daemon.rs:35
- crates/agmem-server/tests/oneshot.rs:24
- crates/agmem-server/tests/stdout_silence.rs:42,83 (comments :37,:57-58,:80)
- crates/agmem-server/tests/reindex.rs:229 (+ assertion :244, unit :209-215)
- scripts/desc-eval.nu:1400
- crates/agmem-server/tests/harness/mod.rs:68-69 (in-process, clap only)

Delete the value outright and every one of those either downloads a model or
fails. So the kind must survive as a *hidden test lever*, or CI has to start
caching a real model (rejected — see below).

Recommendation: rename `EmbedderKind::None` -> `EmbedderKind::Noop`, mark it
`#[value(hide = true)]`, doc it as "test fixture, not a deployment". The
rename makes every stale `--embedder none` invocation fail loudly, which is
what "no longer supported" should feel like; hiding keeps it out of `--help`,
README and design.md. It keeps working through the daemon handshake
(daemon/mod.rs:128,347), doctor's BM25 lines (doctor.rs:108,174) and the
reindex refusal (reindex.rs:109-110) with no structural change.

Rejected alternative: pre-download BGE-small into an actions/rust-cache dir
and run those seven files on the real model. It deletes the hidden-variant
awkwardness but makes CI network-dependent on HuggingFace, adds ~35 MB and a
model load to six process spawns, and reverses design §7's "CI must never
download ONNX models" — which is the rule that keeps the suite reproducible.

## Fixture growth for protocol.rs

`RecordedEmbedder` (crates/agmem-server/tests/harness/recorded.rs:45-49)
**panics** on any unrecorded text, deliberately. Its fixture is produced by
`regenerate_eval_vectors`
(crates/agmem-embed/tests/fastembed.rs:139-227), which walks the *eval
scenario JSON* — `seeds.content`, `seeds.episode`, `gate.candidate` as
passages; `probes/abstain/temporal/timeline/context .query` as queries — and
rewrites `crates/agmem-server/tests/fixtures/eval/vectors.json` from scratch
(51 passages, 23 queries, 347 KB).

That recorder cannot serve protocol.rs: its texts are Rust literals inside
`json!` blocks, plus episode text that `chunk::chunk` splits before
`remember` embeds it (crates/agmem-server/src/tools/remember.rs:248,482).
Nothing static can enumerate them.

**Recommended mechanism: runtime capture + a fold step.**

1. `harness/recorded.rs` loads a *slice* of fixtures and merges them:
   `fixtures/eval/vectors.json` (unchanged, scenario-recorded) and a new
   `fixtures/protocol/vectors.json` (capture-recorded). Same schema
   (`model`, `dim`, `passages`, `queries`), so one `Recording` type and one
   loader serve both; a text present in either resolves.
2. Miss behaviour becomes mode-dependent. Default (CI, and every normal
   `cargo test`): panic, exactly as today. With `AGMEM_RECORD_VECTORS=<path>`
   set: load `FastembedBackend` once behind a `OnceLock`, embed the miss,
   memoize it, and append one JSON line `{"kind","text","vector"}` to the
   journal at `<path>` under a `Mutex<File>`. Journal, not the fixture,
   because the protocol suite runs many tests concurrently in one process and
   `cargo test` may run several test binaries at once.
3. An ignored fold test — `#[test] #[ignore] fn fold_recorded_vectors()` at
   the end of protocol.rs — reads the journal, merges it into
   `fixtures/protocol/vectors.json` through a BTreeMap, and rewrites the file
   with the *same* shortest-round-trip float formatting `regenerate_eval_
   vectors` uses (`format!("{x}")` per f32). Do the merge in Rust, not nu:
   a JSON round-trip through f64 will not reproduce the committed bytes.
4. Wrapper `scripts/record-vectors.nu`: clear the journal, run
   `AGMEM_RECORD_VECTORS=... cargo test -p agmem-server --test protocol`
   (failures expected on the first pass — an assert that trips stops that
   test before its later texts are embedded), run it twice, then run the fold
   test. Idempotent: a second capture pass with a complete fixture writes
   nothing new.

Rejected: a deterministic hash-derived pseudo-vector (no real semantics — the
point of the change is real ones); ast-grep-harvesting the string literals
(misses `format!`-built text and every episode chunk).

Cost: ~170 distinct texts (114 `"content"`, 10 `"insight"`, 19 `"query"`, 23
episodes plus their chunks) at ~4.7 KB per 384-d vector -> a ~1 MB committed
fixture. Worth stating out loud before the first commit.

## Ordered build sequence

Each step leaves `cargo check --workspace --all-targets` green (default
features; the `--no-default-features` line is deleted in step 6).

1. **De-cfg the embed crate.**
   - crates/agmem-embed/src/lib.rs:16-17 — drop `#[cfg(feature = "onnx")]`
     over `pub mod fastembed`; reword the crate doc at :3-4 ("feature `onnx`,
     default" -> unconditional; the no-op is a test fixture).
   - crates/agmem-embed/src/noop.rs:1 — reword `//! The backend for
     `--embedder none`` -> a test-only dimensionless fixture; keep the type.
   - crates/agmem-embed/tests/fastembed.rs:7 — drop `#![cfg(feature="onnx")]`.
   - crates/agmem-server/tests/knn_probe.rs:18 — same (all three of its tests
     are already `#[ignore]`d).
   - crates/agmem-server/src/embedder.rs:14-36 — `build` returns
     `FastembedBackend` for `Fastembed` with no `cfg` arms; delete the
     `#[cfg(not(feature="onnx"))]` bail and the `expect(dead_code)` on
     `model_cache_dir` (:31-34).

2. **Hide the noop kind.**
   - crates/agmem-server/src/config.rs:153-168 — `None` -> `Noop`,
     `#[value(hide = true)]`, `as_str` -> `"noop"`, doc it as a test fixture;
     :56 keeps `default_value_t = EmbedderKind::Fastembed`, help text drops
     the mention of a BM25 mode.
   - crates/agmem-server/src/embedder.rs:16 and
     crates/agmem-server/src/daemon/mod.rs:347 follow the rename.
   - crates/agmem-server/src/reindex.rs:109-110 — refusal names
     `--embedder noop`; keep the `dim > 0` guard (it also protects a caller
     handed a stub).
   - crates/agmem-server/src/doctor.rs:108,174 — the "none (BM25-only mode)"
     lines become "noop (test fixture; no vectors)".

3. **Point every child-process test at the new spelling.** The site list in
   "The constraint" above, plus reindex.rs:209-215 and :244 assertions,
   scripts/desc-eval.nu:1400, harness/mod.rs:68-69,
   scripts/ci-local.nu:23 (drop the `--no-default-features` command) and :27
   (drop the `"no-onnx"` label). Rename the two protocol tests that pin the
   dimensionless path so they read as fixture coverage: protocol.rs:788
   `without_an_embedder_the_exact_gate_still_holds`, :1499
   `a_deployment_with_no_vectors_never_abstains` — both **stay** on
   `NoopEmbedder`.

4. **Grow the harness.** crates/agmem-server/tests/harness/recorded.rs — the
   two-fixture merge, the capture mode, the fold helper (mechanism above);
   new empty-ish `crates/agmem-server/tests/fixtures/protocol/vectors.json`;
   new `scripts/record-vectors.nu`. Nothing else changes yet, so eval.rs
   still passes untouched — that is the check that the merge did not move the
   eval numbers.

5. **Flip protocol.rs.** 52 of the 54 `Harness::start(Arc::new(NoopEmbedder))`
   sites -> `RecordedEmbedder`; leave :788 and :1499. Leave every
   `KeywordEmbedder` (14 sites) and `AngleEmbedder` (7 sites) alone — they
   exist to hit exact cosines that no real model reaches. Record, then fix
   the at-risk assertions (list below). Expect two or three record/run
   iterations.

6. **CI and manifests.**
   - .github/workflows/ci.yml:43-44 — delete the `--no-default-features`
     check and its comment; :10-11 keep `FASTEMBED_CACHE_DIR: /nonexistent`
     and reword the comment (still true, now enforced by the noop fixture).
   - crates/agmem-embed/Cargo.toml:10-21 — drop `default`/`onnx`; `fastembed`
     stops being `optional`; `rerank` becomes `rerank = []` (it is
     independent once fastembed is unconditional — it only gates the ~150 MB
     reranker model tests); :3 description reworded.
   - crates/agmem-server/Cargo.toml:15-19 — delete the `[features]` block.
   - Cargo.toml:26-27 — `agmem-embed` drops `default-features = false`;
     rewrite the comment at :26.

7. **Docs.**
   - README.md:34-37 (drop the `--no-default-features` advice; keep the Intel
     mac build-from-source note), :556 (`--embedder` row -> `fastembed`
     only), :681-685 (the "No ONNX Runtime on the platform" bullet — it now
     says the model is required and what a mismatch does), :694 (CI list
     drops the BM25-only build check).
   - docs/design.md:229 (`option<…>` is for pre-embed rows and interrupted
     reindexes, not a mode), :610 (noop.rs comment), :609 (drop the feature
     tag), :1061 (`--embedder` row), :1136-1140 (risk 1 — the
     `--no-default-features` contingency is gone; say what replaces it:
     pin `ort` exactly and treat a broken platform as a release blocker),
     :1229 (the `similarity: 1.0` branch is now only reachable in tests),
     :752 / :833 (reword "BM25-only mode" -> "rows without a vector").
   - Lower-priority prose carrying the same phrase, safe to leave for a
     follow-up: crates/agmem-core/src/model.rs:499,585,
     crates/agmem-core/src/scoring.rs:279,
     crates/agmem-store/src/{migrate.rs:62-63,158, queries/read.rs:449,
     repo/read.rs:448, repo/write.rs:36,88},
     crates/agmem-server/src/tools/{mod.rs:142-145, remember.rs:200,210,495,
     recall.rs:378,394, consolidate.rs:118, abstain.rs:27,204},
     crates/agmem-store/tests/reindex.rs:31.

8. **Release config — nothing to do.** dist-workspace.toml names no features
   (`installers`, `tap`, `formula = "agmem"`, the glibc note at :31);
   release-plz.toml is version/tag policy only; .github/workflows/release.yml
   builds with default features. Confirm with a `dist plan` after step 6.

## Tests at risk when the vectors become real

Abstention (`MIN_SIMILARITY = 0.62`, tools/abstain.rs:46) **clears the whole
page** when the best measured similarity falls under it, and the recorded-BGE
unrelated floor sits at 0.54-0.60 — so any protocol recall whose `query` is
only loosely related to its seeds now returns nothing. That, not ordering, is
the biggest source of breakage.

Reads with a `query` (abstain floor + ranking + occupancy cap):
- a_temporal_window_rescores_without_hiding_anything (:1396)
- recall_unions_the_current_space_with_the_user_space (:1584)
- as_of_returns_the_claim_that_was_live_then (:1630)
- as_of_reads_only_episodes_that_had_already_happened (:1696)
- one_dominant_episode_cannot_flood_a_page (:1774)  <- occupancy cap
- a_full_page_says_how_much_of_the_store_it_is_not (:1854)
- a_returned_memory_is_reinforced_and_a_k_past_the_ceiling_is_refused (:1892)
- a_historical_read_reinforces_nothing (:1933)
- context_lays_out_the_sections_in_a_fixed_order_and_reinforces_nothing
  (:1972), a_small_budget_keeps_the_first_section_and_says_it_trimmed
  (:2055), a_flooded_tag_yields_lessons_slots_and_recall_stays_uncapped
  (:2090), an_empty_space_says_so_and_an_unusable_budget_is_refused (:2130)
- a_reflection_is_recallable_and_walks_back_to_its_evidence (:3061)
- a_summary_stands_in_for_its_children_and_expands_on_demand (:3193)
- the hop group, which seeds off the ranked page: chain_store (:3440),
  nothing_to_hop_from_changes_nothing (:3500), a_hub_entity_never_seeds
  (:3555), a_full_page_reserves_its_last_slot_for_the_hop (:3584)

Write path — `NEAR_DUP_THRESHOLD` 0.95 refuses, `CORRECTION_FLOOR` 0.75 now
populates `related` where noop left it empty (agmem-core/src/dedup.rs:14,65):
- a_write_records_its_writer_and_inspect_shows_it (:569)
- a_meta_session_override_is_what_the_writer_records (:612)
- inspect_walks_a_chain_of_two_corrections_oldest_first (:2161)
- a_claim_links_back_to_the_text_it_was_distilled_from (:2219)
- plus any test asserting `created` for two paraphrases of one claim

Consolidate — the `near_duplicates` (0.90) and `contradictions` (0.75-…)
arms now return entries that were empty under noop:
- consolidate_reports_short_lived_notes_that_recall_kept_alive (:2871)
- consolidate_reports_a_tag_holding_more_lessons_than_the_bound (:2917)

Unaffected (no vector is read): the initialize/list/resource/prompt snapshot
tests (:31-:476), write_side_space_keywords_resolve_instead_of_becoming_slugs
(:1017), a_request_that_cannot_be_stored_names_what_is_wrong (:1204),
an_inverted_window_is_refused (:1476), the inspect-grammar and forget group
(:2360, :2462, :2520, :2567, :2599, :2653), a_reflection_has_to_cite_
something_the_store_holds (:3141).

## Other test files

- **reindex.rs** — touch: :209-215 keeps `NoopEmbedder` as the dimensionless
  case but its assertion string follows the rename; :229 and :244 follow it
  too. The rest (`Stub` at :21-55) is untouched and still the right pattern.
- **eval.rs** — the noop canary
  `retrieval_without_vectors_scores_strictly_worse` (:71-89) **stays exactly
  as it is**. It is the sensitivity check, and it needs a dimensionless
  embedder to be the control; that is precisely the fixture role NoopEmbedder
  keeps.
- **stdout_silence.rs** — touch, spelling only (:42,:83 and the comments at
  :37,:57-58,:80, which currently explain the choice in terms of a shipped
  mode). `the_real_embedder_writes_nothing_to_stdout_either` (:65) is already
  `#[ignore]`d and unchanged.
- **knn_probe.rs** — touch line 18 only (drop the feature gate); its three
  tests are all `#[ignore]`d and keep working.
- **doctor.rs, stdio.rs, ws.rs, daemon.rs, oneshot.rs** — spelling only.
