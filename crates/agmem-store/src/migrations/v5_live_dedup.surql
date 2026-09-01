-- Schema v5 — the exact-duplicate gate stops seeing the dead (issue #61).
--
-- `mem_hash` was UNIQUE over `(space, content_hash)` with no regard for
-- liveness, which made two promises at once and could only keep one:
-- supersession says a closed claim stays on disk, and the index says its
-- hash may never appear again. So content that was superseded or forgotten
-- could never be re-asserted — the dead row answered as the duplicate, and
-- the documented retry ("re-send with the id in `supersedes`") then closed
-- live rows in favour of a dead one.
--
-- The index cannot simply gain a liveness clause — an index has no WHERE —
-- and it cannot move to `(space, content_hash, invalid_at)` either, because
-- a unique index skips rows where a column is NONE (SQL NULL semantics), so
-- two *live* duplicates would both land and the race protection the old
-- index provided (issue #64) would silently vanish. Hence `dedup_key`: a
-- computed field that reads 'live' while `invalid_at` is NONE and the close
-- time once it is set. One live row per `(space, content_hash)` stays
-- enforced — every live row shares the 'live' key — while closed rows
-- coexist, each keyed by when it was closed. VALUE recomputes on every
-- write, so a supersede or forget moves the row out of the 'live' slot in
-- the same statement that closes it, and nothing can drift.
--
-- The bare UPDATE is the backfill: writing a row is what makes a VALUE
-- field compute, and it must run before the new index builds so no row
-- enters it keyless. Two closes of the same content at the same instant
-- would collide in the new index; closes take their boundary from distinct
-- successors' `valid_from`, so that needs two writers landing the same
-- nanosecond — the transaction abort it would cause is acceptable where
-- silently resurrecting history is not.
DEFINE FIELD IF NOT EXISTS dedup_key ON memory TYPE string VALUE <string>(invalid_at ?? 'live');
UPDATE memory;
REMOVE INDEX IF EXISTS mem_hash ON TABLE memory;
DEFINE INDEX IF NOT EXISTS mem_hash_live ON memory COLUMNS space, content_hash, dedup_key UNIQUE;
