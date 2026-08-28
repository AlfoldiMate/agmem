-- Schema v1 — the full v1 data model (docs/design.md §2.2).
--
-- Every statement is IF NOT EXISTS so a half-applied batch re-runs cleanly.
-- Record IDs are minted with ulid() by the write path: ULIDs sort temporally,
-- which makes "recent N" a range scan and pagination stable.

-- space registry: isolation is a field, not a database
DEFINE TABLE IF NOT EXISTS space SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS name ON space TYPE string;
DEFINE FIELD IF NOT EXISTS created_at ON space TYPE datetime DEFAULT time::now();
DEFINE INDEX IF NOT EXISTS space_name ON space COLUMNS name UNIQUE;

DEFINE ANALYZER IF NOT EXISTS english TOKENIZERS class FILTERS lowercase, snowball(english);

-- episode: verbatim ground truth, append-only
DEFINE TABLE IF NOT EXISTS episode SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS space ON episode TYPE string;
DEFINE FIELD IF NOT EXISTS content ON episode TYPE string;
DEFINE FIELD IF NOT EXISTS content_hash ON episode TYPE string;          -- blake3
DEFINE FIELD IF NOT EXISTS occurred_at ON episode TYPE datetime DEFAULT time::now();
DEFINE FIELD IF NOT EXISTS session ON episode TYPE option<string>;
DEFINE FIELD IF NOT EXISTS created_at ON episode TYPE datetime DEFAULT time::now();
DEFINE INDEX IF NOT EXISTS episode_hash ON episode COLUMNS space, content_hash UNIQUE;

-- episode_chunk: retrieval unit for the verbatim side
DEFINE TABLE IF NOT EXISTS episode_chunk SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS episode ON episode_chunk TYPE record<episode>;
DEFINE FIELD IF NOT EXISTS space ON episode_chunk TYPE string;
DEFINE FIELD IF NOT EXISTS text ON episode_chunk TYPE string;
DEFINE FIELD IF NOT EXISTS position ON episode_chunk TYPE int;
DEFINE FIELD IF NOT EXISTS embedding ON episode_chunk TYPE option<array<float>>;
DEFINE INDEX IF NOT EXISTS ec_ft ON episode_chunk COLUMNS text FULLTEXT ANALYZER english BM25 HIGHLIGHTS;
DEFINE INDEX IF NOT EXISTS ec_vec ON episode_chunk FIELDS embedding HNSW DIMENSION 384 DIST COSINE;

-- memory: distilled, supersedable records
DEFINE TABLE IF NOT EXISTS memory SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS space ON memory TYPE string;
DEFINE FIELD IF NOT EXISTS kind ON memory TYPE string
    ASSERT $value IN ["fact", "lesson", "instruction"];
DEFINE FIELD IF NOT EXISTS content ON memory TYPE string;
DEFINE FIELD IF NOT EXISTS content_hash ON memory TYPE string;           -- blake3(normalized)
DEFINE FIELD IF NOT EXISTS entities ON memory TYPE array<string> DEFAULT [];
DEFINE FIELD IF NOT EXISTS tags ON memory TYPE array<string> DEFAULT [];
DEFINE FIELD IF NOT EXISTS embedding ON memory TYPE option<array<float>>;
DEFINE FIELD IF NOT EXISTS decay_class ON memory TYPE string DEFAULT "normal"
    ASSERT $value IN ["pinned", "slow", "normal", "fast"];
DEFINE FIELD IF NOT EXISTS strength ON memory TYPE float DEFAULT 1.0;    -- Ebbinghaus S
DEFINE FIELD IF NOT EXISTS last_accessed ON memory TYPE datetime DEFAULT time::now();
DEFINE FIELD IF NOT EXISTS access_count ON memory TYPE int DEFAULT 0;
DEFINE FIELD IF NOT EXISTS valid_from ON memory TYPE datetime DEFAULT time::now();
DEFINE FIELD IF NOT EXISTS invalid_at ON memory TYPE option<datetime>;   -- none = live
DEFINE FIELD IF NOT EXISTS invalid_reason ON memory TYPE option<string>
    ASSERT $value IN ["superseded", "forgotten", "expired"];
DEFINE FIELD IF NOT EXISTS supersedes ON memory TYPE option<record<memory>>;
DEFINE FIELD IF NOT EXISTS superseded_by ON memory TYPE option<record<memory>>;
DEFINE FIELD IF NOT EXISTS source ON memory TYPE object;
DEFINE FIELD IF NOT EXISTS source.kind ON memory TYPE string
    ASSERT $value IN ["episode", "agent", "external"];
DEFINE FIELD IF NOT EXISTS source.ref ON memory TYPE option<record<episode> | string>;
DEFINE FIELD IF NOT EXISTS created_at ON memory TYPE datetime DEFAULT time::now();
DEFINE INDEX IF NOT EXISTS mem_hash ON memory COLUMNS space, content_hash UNIQUE;
-- `.*` indexes each element, not the array as one value: with it,
-- `entities CONTAINS $x` is an IndexScan and `entities = $x` a table scan
-- returning nothing; without it, exactly the other way round.
DEFINE INDEX IF NOT EXISTS mem_entities ON memory COLUMNS entities.*;
DEFINE INDEX IF NOT EXISTS mem_tags ON memory COLUMNS tags.*;
DEFINE INDEX IF NOT EXISTS mem_ft ON memory COLUMNS content FULLTEXT ANALYZER english BM25 HIGHLIGHTS;
DEFINE INDEX IF NOT EXISTS mem_vec ON memory FIELDS embedding HNSW DIMENSION 384 DIST COSINE;
