-- Schema v9 — documents: a named, typed artifact over the episode ground
-- truth (issue #132).
--
-- An episode already is the immutable, content-hashed, chunked and indexed
-- raw tier every memory system converges on; what it lacks to serve as a
-- *document* — a plan, a review, a probe report — is a name and a type. So
-- a document is an episode row with `doc_kind` set, not a second table: one
-- retrieval path, one purge path, one `derived_from` target. `title` is
-- indexed with `space` but not unique — title supersession (#134) keeps
-- several rows per title, one of them live by convention, not by schema.
--
-- The citation span cannot ride `derived_from`: that field is a bare
-- `array<record<memory | episode>>` and carries no payload. It lives in a
-- sidecar `spans` on `memory` keyed by the episode it points into. Offsets
-- are Unicode scalar (char) offsets into the verbatim `content`, `end`
-- exclusive — the unit the 100k-char cap counts, never the normalized text
-- the hash is taken over. Nothing writes it yet; it lands here because
-- provenance is the field that can never be backfilled (the v6 lesson).
--
-- Deliberately **no backfill and no UPDATE**: a pre-v9 episode reads as
-- anonymous, which is true. Every field is `option<>` because a SurrealDB
-- UPDATE re-coerces every field (the v3 lesson), and memory rows are updated
-- by REINFORCE, PRUNE_EXPIRED, FORGET_SOFT and SUPERSEDE.
DEFINE FIELD IF NOT EXISTS title ON episode TYPE option<string>;
DEFINE FIELD IF NOT EXISTS doc_kind ON episode TYPE option<string>
    ASSERT $value IN ["plan", "review", "report", "probe", "transcript", "other"];
DEFINE FIELD IF NOT EXISTS tags ON episode TYPE option<array<string>>;
DEFINE FIELD IF NOT EXISTS mime ON episode TYPE option<string>;
DEFINE INDEX IF NOT EXISTS ep_title ON episode COLUMNS space, title;
DEFINE INDEX IF NOT EXISTS ep_tags ON episode COLUMNS tags.*;

DEFINE FIELD IF NOT EXISTS spans ON memory TYPE option<array<object>>;
DEFINE FIELD IF NOT EXISTS spans.*.ref ON memory TYPE record<episode>;
DEFINE FIELD IF NOT EXISTS spans.*.start ON memory TYPE int;
DEFINE FIELD IF NOT EXISTS spans.*.end ON memory TYPE int;
