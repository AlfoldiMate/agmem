-- Schema v4 — episode chunks learn when they happened.
--
-- `as_of` asks what was known at an instant. The memory arms answer through
-- `valid_from`/`invalid_at`, but chunk rows carried no date at all, so a
-- chunk stored today came back — ranked first — for an `as_of` years before
-- its episode occurred. The episode row has always known its `occurred_at`;
-- this copies it onto the retrieval unit, the same denormalisation `space`
-- already rides: a chunk is queried without its episode, so what the query
-- filters on has to sit on the chunk. The copy cannot drift — an episode is
-- append-only, never updated, and a content-hash duplicate writes no new
-- chunks.
--
-- `option<datetime>`, not required-with-a-default: `repo::reindex` UPDATEs
-- every chunk row to re-embed it, and an UPDATE re-coerces every schemafull
-- field on the row, mentioned or not (v3's lesson) — a required field a row
-- lacks would fail that write. The UPDATE below is the backfill; a row it
-- somehow misses reads NONE, and `NONE <= $as_of` is falsy, so it drops out
-- of as-of results rather than lying about its date.
DEFINE FIELD IF NOT EXISTS occurred_at ON episode_chunk TYPE option<datetime>;
UPDATE episode_chunk SET occurred_at = episode.occurred_at WHERE occurred_at IS NONE;
