-- Schema v2 — derivation provenance (docs/design.md §2.2, issue #26).
--
-- A reflection is just another memory row; what makes it one is the evidence
-- it cites. The link is typed to the two tables an insight can be drawn from,
-- so a citation cannot name a chunk or a space by accident, and `record::id`
-- follows it the same way it follows `supersedes`.
--
-- Rows written before this migration carry no `derived_from` at all: a DEFAULT
-- applies to writes, not to rows already on disk. Every read therefore
-- projects `derived_from ?? []` rather than trusting the column to be there.
DEFINE FIELD IF NOT EXISTS derived_from ON memory
    TYPE array<record<memory | episode>> DEFAULT [];
