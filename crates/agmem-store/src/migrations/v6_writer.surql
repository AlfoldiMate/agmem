-- Schema v6 — every write records who made it (issue #75).
--
-- `source` says where the *content* came from; nothing says which client put
-- it in the store. Every poisoning defense and outcome-feedback idea
-- downstream needs per-writer attribution, and it is the one field that can
-- never be backfilled — so it lands before the store grows further.
--
-- `writer` is an object of plain strings: the client name and version from
-- the MCP `initialize` handshake, the session the write belonged to, and the
-- agmem tool that performed it. Episodes record it too — verbatim text is
-- just as un-backfillable as a claim.
--
-- Deliberately **no backfill**: a pre-v6 row reads as `NONE`, which is true
-- ("not recorded"), where any sentinel would be a claim to knowledge nothing
-- has. Every sub-field is `option<string>` for the same reason v3 taught: any
-- later UPDATE re-coerces every field, and a required sub-field would make
-- REINFORCE, PRUNE_EXPIRED and FORGET_SOFT refuse to touch a pre-v6 row.
DEFINE FIELD IF NOT EXISTS writer ON memory TYPE option<object>;
DEFINE FIELD IF NOT EXISTS writer.client ON memory TYPE option<string>;
DEFINE FIELD IF NOT EXISTS writer.client_version ON memory TYPE option<string>;
DEFINE FIELD IF NOT EXISTS writer.session ON memory TYPE option<string>;
DEFINE FIELD IF NOT EXISTS writer.tool ON memory TYPE option<string>;
DEFINE INDEX IF NOT EXISTS mem_writer_session ON memory COLUMNS writer.session;
DEFINE FIELD IF NOT EXISTS writer ON episode TYPE option<object>;
DEFINE FIELD IF NOT EXISTS writer.client ON episode TYPE option<string>;
DEFINE FIELD IF NOT EXISTS writer.client_version ON episode TYPE option<string>;
DEFINE FIELD IF NOT EXISTS writer.session ON episode TYPE option<string>;
DEFINE FIELD IF NOT EXISTS writer.tool ON episode TYPE option<string>;
