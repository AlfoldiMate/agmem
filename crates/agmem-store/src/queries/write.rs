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
