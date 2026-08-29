-- Schema v3 — `supersedes` becomes a list (docs/design.md §2.2, issue #42).
--
-- One id per memory made a merge unachievable: three wordings of one claim
-- could be replaced by one surviving row only by superseding a single member
-- and `forget`ting the rest, which closes those rows *and* takes their
-- correction history with them. Measured in `docs/eval/consolidate-write/`:
-- 11 of 13 closures were forgotten, one supersession per run.
--
-- `superseded_by` stays singular. A merge is many claims closed by one
-- survivor, so each closed row still has exactly one successor to point at,
-- and the forward walk is unchanged. Only the backwards walk widens.
--
-- The redefine does not touch rows already on disk (a TYPE, like a DEFAULT,
-- applies to writes), so the two UPDATEs below are the migration: they wrap
-- the single link v1 wrote into a one-element list and give every other row
-- the empty one. Re-running is safe — after the first pass no row is left
-- holding a record or a NONE here.
--
-- This is the first migration to *write* to rows already on disk, and an
-- `UPDATE` re-coerces every schemafull field on the row it touches, not only
-- the ones being set. So it also has to fill in `derived_from`, which rows
-- written before v2 do not carry at all: without that, the upgrade fails with
-- "Expected `array` but found `NONE`" on the column this statement never
-- mentions. Any later migration that writes to old rows inherits the same
-- rule. It rewrites the whole `memory` table once, at the one startup that
-- crosses this version.
DEFINE FIELD OVERWRITE supersedes ON memory TYPE array<record<memory>> DEFAULT [];
UPDATE memory SET derived_from = derived_from ?? [], supersedes = [supersedes]
    WHERE type::is_record(supersedes);
UPDATE memory SET derived_from = derived_from ?? [], supersedes = []
    WHERE supersedes IS NONE;
