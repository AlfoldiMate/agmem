//! The SurrealQL behind the write path (design §5.2).
//!
//! Two engine behaviours dictate the shape of a batch:
//!
//! - A unique-index conflict inside a transaction aborts the **whole**
//!   transaction, so an exact duplicate can never be allowed to reach the
//!   index. Every insert is guarded by a lookup on `(space, content_hash)` in
//!   the same transaction, and the guard's `ELSE` branch is the only place a
//!   `CREATE` appears. That also dedups *within* a batch.
//! - A `RETURN` inside a block assigned with `LET` returns from the entire
//!   query, silently skipping every later statement. Blocks below therefore
//!   end in a bare expression, which is the block's value.

use agmem_core::MemoryId;

use super::{Builder, Script};

/// What the query text needs to know about one memory; everything else about
/// it travels as a bound parameter.
pub(crate) struct MemoryShape<'a> {
    /// Whether provenance is the episode this same transaction writes — whose
    /// ULID exists only as a SurrealQL variable, so it cannot be bound.
    pub(crate) source_is_batch_episode: bool,
    /// The memory this one closes, if any.
    pub(crate) supersedes: Option<&'a MemoryId>,
}

/// The whole write of one `remember` call: an optional episode with its
/// chunks, then the memories, then any supersessions — atomically.
pub(crate) fn insert_batch(memories: &[MemoryShape<'_>], with_episode: bool) -> Script {
    let mut builder = Builder::transaction();
    builder.push("LET $out = []");

    if with_episode {
        builder.push(
            "LET $ep_dup = (SELECT VALUE id FROM episode
                 WHERE space = $space AND content_hash = $ep_hash LIMIT 1)[0]",
        );
        builder.push(
            "LET $ep_res = IF $ep_dup IS NOT NONE {
                 { id: $ep_dup, created: false }
             } ELSE {
                 LET $ep_new = (CREATE ONLY episode:ulid() CONTENT $ep_row);
                 FOR $chunk IN $ep_chunks {
                     CREATE episode_chunk:ulid() CONTENT {
                         episode: $ep_new.id, space: $space, text: $chunk.text,
                         position: $chunk.position, embedding: $chunk.embedding
                     }
                 };
                 { id: $ep_new.id, created: true }
             }",
        );
    }

    for (index, shape) in memories.iter().enumerate() {
        let source = if shape.source_is_batch_episode {
            "{ kind: 'episode', ref: $ep_res.id }".to_owned()
        } else {
            format!("$src{index}")
        };
        let supersede = match shape.supersedes {
            // A ULID is Crockford base32, so it cannot break out of the
            // literal; the id is baked in to name the offender in the error.
            Some(old) => format!(
                "LET $sup{index} = (UPDATE $old{index} SET superseded_by = $new{index}.id,
                     invalid_at = $new{index}.valid_from, invalid_reason = 'superseded');
                 IF array::len($sup{index}) = 0 {{
                     THROW 'supersedes target memory:{old} does not exist'
                 }};"
            ),
            None => String::new(),
        };
        builder.push(format!(
            "LET $dup{index} = (SELECT VALUE id FROM memory
                 WHERE space = $space AND content_hash = $hash{index} LIMIT 1)[0]"
        ));
        builder.push(format!(
            "LET $res{index} = IF $dup{index} IS NOT NONE {{
                 {{ id: record::id($dup{index}), created: false }}
             }} ELSE {{
                 LET $new{index} = (CREATE ONLY memory:ulid()
                     CONTENT object::extend($row{index}, {{ source: {source} }}));
                 {supersede}
                 {{ id: record::id($new{index}.id), created: true }}
             }}"
        ));
        builder.push(format!("LET $out = array::append($out, $res{index})"));
    }

    let episode = if with_episode {
        "{ id: record::id($ep_res.id), created: $ep_res.created }"
    } else {
        "NONE"
    };
    builder.finish(format!("RETURN {{ episode: {episode}, memories: $out }}"))
}

/// Register `$name` in the space table unless it is registered already.
///
/// Guarded rather than blind for the same reason every insert is: `space_name`
/// is UNIQUE, and a conflict inside a transaction aborts the whole thing.
pub(crate) const ENSURE_SPACE: &str = "BEGIN;
LET $existing = (SELECT VALUE id FROM space WHERE name = $name LIMIT 1)[0];
IF $existing IS NONE { CREATE space:ulid() SET name = $name };
COMMIT;";

/// Which of `$ids` exist in `$space`, as bare ULIDs.
pub(crate) const EXISTING_MEMORIES: &str =
    "SELECT VALUE record::id(id) FROM memory WHERE space = $space AND id IN $ids";

/// Close `$old` in favour of `$new`, taking the boundary from the successor so
/// the history chain has no gap and no overlap.
pub(crate) const SUPERSEDE: &str = "BEGIN;
LET $valid_from = (SELECT VALUE valid_from FROM $new)[0];
IF $valid_from IS NONE { THROW 'the superseding memory does not exist' };
LET $done = (UPDATE $old SET superseded_by = $new,
    invalid_at = $valid_from, invalid_reason = 'superseded');
IF array::len($done) = 0 { THROW 'the superseded memory does not exist' };
COMMIT;";

/// Close every live memory in `$memories` that one of `$spaces` holds
/// (design §5.4).
///
/// `invalid_at IS NONE` is what keeps a forget from rewriting history: a
/// memory a correction already closed keeps its own reason and its own date,
/// and its id simply does not come back. The `space` clause keeps an id a
/// capability inside its space, exactly as every read does.
pub(crate) const FORGET_SOFT: &str = "UPDATE $memories
     SET invalid_at = time::now(), invalid_reason = 'forgotten'
     WHERE space IN $spaces AND invalid_at IS NONE
     RETURN VALUE record::id(id)";

/// Close every live memory of `$class` whose retention has fallen past the
/// prune threshold (design §2.3, §5.5).
///
/// The decay curve is not repeated here. `$horizon` is the idle time at which
/// unit strength reaches the threshold (`core::scoring::decay_horizon_secs`)
/// and a row's own horizon is that scaled by its `strength`, floored at
/// `$floor` the way the Rust formula floors it.
///
/// The comparison is written forwards — `last_accessed + horizon < now` rather
/// than `now − last_accessed > horizon` — because SurrealDB durations are
/// unsigned: subtracting a `last_accessed` in the future either errors the
/// whole statement ("the operation results in a negative value") or comes back
/// as a large *positive* duration, and either one expires a clock-skewed row
/// that has barely aged. Adding to the row's own timestamp has no such edge.
pub(crate) const PRUNE_EXPIRED: &str = "UPDATE memory
     SET invalid_at = time::now(), invalid_reason = 'expired'
     WHERE decay_class = $class AND invalid_at IS NONE
       AND last_accessed
           + duration::from_secs(<int> math::round($horizon * math::max([strength, $floor])))
           < time::now()
     RETURN VALUE record::id(id)";

/// Delete rows outright, an episode's slices along with it (design §5.4).
///
/// `DELETE … RETURN VALUE record::id(id)` is an *error*, not an empty list —
/// the projection runs against the row after deletion, where `id` is NONE and
/// `record::id` refuses it. The rows are therefore taken whole with `RETURN
/// BEFORE` and projected afterwards.
///
/// Nothing here resolves an id or expands a supersession chain: the caller
/// hands over exactly the rows it means, having already shown them to the
/// agent. An empty list is a clean no-op on both statements.
pub(crate) fn forget_purge() -> Script {
    let mut builder = Builder::transaction();
    builder.push(
        "LET $chunks = (DELETE episode_chunk
             WHERE space IN $spaces AND episode IN $episodes RETURN BEFORE)",
    );
    builder.push("LET $gone_episodes = (DELETE $episodes WHERE space IN $spaces RETURN BEFORE)");
    builder.push("LET $gone_memories = (DELETE $memories WHERE space IN $spaces RETURN BEFORE)");
    builder.finish(
        "RETURN { chunks: array::len($chunks),
             episodes: $gone_episodes.map(|$row| record::id($row.id)),
             memories: $gone_memories.map(|$row| record::id($row.id)) }"
            .to_owned(),
    )
}
