//! `remember` — the write verb (design §3.1, §5.2).
//!
//! The flow is Mem0's ADD/UPDATE/NOOP loop with the decision moved to the only
//! place an LLM exists: the caller. agmem measures, reports, and stores exactly
//! what it was given. It never merges two claims, never rewrites one, and never
//! decides that a near-duplicate was meant as a correction — that is what
//! `supersedes` is for, and the agent is the one that sends it.
//!
//! So the interesting output is the *diff*: which claims were stored, which
//! were already there (with the id and how close a match), and which older
//! memories got closed. An agent that reads the diff can decide what to do
//! next; an agent that gets a silent success cannot.

use std::sync::Arc;

use agmem_core::{DecayClass, Kind, MemoryId, SpaceName, chunk, dedup};
use agmem_store::repo::{self, Batch, NewChunk, NewEpisode, NewMemory, Written};
use jiff::Timestamp;
use rmcp::ErrorData;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::service::AgmemService;
use crate::tools::{internal, invalid, store_error};

/// One `remember` call: a batch of distilled claims, optionally with the
/// verbatim text they were distilled from.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct RememberParams {
    /// Where to store this. Defaults to the space this server was started
    /// with; `user` is the reserved space for memory that follows the person
    /// across every project.
    #[serde(default)]
    pub space: Option<String>,

    /// The distilled claims, one per entry. May be empty only when `episode`
    /// is present.
    pub memories: Vec<MemoryInput>,

    /// The verbatim text these claims came from, stored unedited as ground
    /// truth. Every memory in the same call is provenanced to it.
    #[serde(default)]
    pub episode: Option<EpisodeInput>,
}

/// One claim to store.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct MemoryInput {
    /// One atomic, self-contained statement in the third person — it has to
    /// still make sense with no conversation around it. "The user prefers Rust
    /// over Python for CLI tools", not "he said he likes it better".
    pub content: String,

    /// What the claim is; defaults to `fact`.
    #[serde(default)]
    pub kind: Option<Kind>,

    /// The subjects this claim is about ("user", "project-x"), for filtered
    /// recall.
    #[serde(default)]
    pub entities: Vec<String>,

    /// Free labels. `identity` marks a fact that belongs in every session's
    /// profile.
    #[serde(default)]
    pub tags: Vec<String>,

    /// How fast this fades between uses. Defaults by kind: facts `normal`,
    /// lessons `slow`, instructions `pinned`.
    #[serde(default)]
    pub decay_class: Option<DecayClass>,

    /// The id of a live memory this claim corrects. The old one is closed and
    /// stays readable and dated; only the correction is live afterwards.
    #[serde(default)]
    pub supersedes: Option<String>,

    /// When the claim started being true, RFC3339. Defaults to now — set it
    /// when recording something that became true earlier.
    #[serde(default)]
    pub valid_from: Option<String>,
}

/// Verbatim ground truth to store alongside what was distilled from it.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct EpisodeInput {
    /// The text, unedited. It is chunked for retrieval but never rewritten.
    pub content: String,

    /// When the events described happened, RFC3339. Defaults to now.
    #[serde(default)]
    pub occurred_at: Option<String>,

    /// A grouping key for one conversation or working session.
    #[serde(default)]
    pub session: Option<String>,
}

/// What the call changed.
#[derive(Debug, Serialize, JsonSchema)]
pub struct RememberResult {
    /// Ids of the memories stored, in the order they were sent, skipping the
    /// positions reported in `duplicates`.
    pub created: Vec<String>,

    /// The claims that were already stored. Nothing was written for these:
    /// accept that as a no-op, or re-send the claim with `supersedes` set to
    /// the id here if it is genuinely a correction.
    pub duplicates: Vec<Duplicate>,

    /// Ids of the memories closed by a `supersedes` in this call.
    pub superseded: Vec<String>,

    /// Id of the episode, whether it was written now or already stored.
    pub episode: Option<String>,
}

/// A claim that was already in the store.
#[derive(Debug, Serialize, JsonSchema)]
pub struct Duplicate {
    /// The id of the memory that already holds this claim.
    pub id: String,

    /// Which entry of the `memories` you sent this refers to, zero-based.
    pub of: usize,

    /// How close the two are, as cosine similarity.
    ///
    /// Text identical to what is stored reads a rounding error *short* of 1.0
    /// — 0.9999998 is typical — because verbatim input trips the vector gate
    /// as its own nearest neighbour before the content hash is ever consulted.
    /// Exactly 1.0 means the hash matched with no embedding to compare it
    /// against, which is BM25-only mode.
    pub similarity: f64,
}

/// Run the write path (design §5.2).
///
/// # Errors
/// [`ErrorData`] with `INVALID_PARAMS` for anything the caller can fix — a bad
/// space slug, an empty claim, an unparseable timestamp, a `supersedes` id
/// this space does not hold — and `INTERNAL_ERROR` for a failing embedder or
/// store.
pub async fn run(
    service: &AgmemService,
    params: RememberParams,
) -> Result<RememberResult, ErrorData> {
    let RememberParams {
        space,
        memories,
        episode,
    } = params;
    let space = resolve_space(service, space.as_deref()).await?;
    if memories.is_empty() && episode.is_none() {
        return Err(invalid(
            "nothing to remember: send at least one memory, or an episode",
        ));
    }

    // 1. Validate everything before anything is embedded or written: a batch
    //    that cannot land whole must not cost a model run.
    let mut new_memories: Vec<NewMemory> = memories
        .iter()
        .enumerate()
        .map(|(index, input)| input.validated(index))
        .collect::<Result<_, ErrorData>>()?;
    let mut new_episode = episode.as_ref().map(EpisodeInput::validated).transpose()?;

    // 2. One embedding pass for the whole call — the claims and the episode's
    //    chunks together, so a batch of any size is one model invocation.
    embed(service, &mut new_memories, new_episode.as_mut()).await?;

    // 3. The near-dup gate: the same claim in different words is reported, not
    //    stored. A memory that carries `supersedes` skips the gate — the agent
    //    has already made the ADD/UPDATE call, and a correction is usually
    //    *close* to what it corrects.
    let gated: Vec<usize> = new_memories
        .iter()
        .enumerate()
        .filter(|(_, memory)| memory.supersedes.is_none() && memory.embedding.is_some())
        .map(|(index, _)| index)
        .collect();
    let probes: Vec<Vec<f32>> = gated
        .iter()
        .filter_map(|&index| new_memories[index].embedding.clone())
        .collect();
    let neighbours = repo::nearest_live(service.db(), &space, &probes)
        .await
        .map_err(|error| store_error(&error))?;

    let mut duplicates: Vec<Duplicate> = Vec::new();
    let mut blocked = vec![false; new_memories.len()];
    for (index, neighbour) in gated.into_iter().zip(neighbours) {
        if let Some(neighbour) = neighbour
            && dedup::is_near_duplicate(neighbour.similarity)
        {
            blocked[index] = true;
            duplicates.push(Duplicate {
                id: neighbour.id.to_string(),
                of: index,
                similarity: neighbour.similarity,
            });
        }
    }

    // 4. One transaction for what survived. All of it duplicate and no episode
    //    means there is nothing to write at all.
    let kept: Vec<usize> = (0..new_memories.len())
        .filter(|index| !blocked[*index])
        .collect();
    let batch: Vec<NewMemory> = new_memories
        .into_iter()
        .zip(&blocked)
        .filter(|(_, blocked)| !**blocked)
        .map(|(memory, _)| memory)
        .collect();
    if batch.is_empty() && new_episode.is_none() {
        duplicates.sort_by_key(|duplicate| duplicate.of);
        return Ok(RememberResult {
            created: Vec::new(),
            duplicates,
            superseded: Vec::new(),
            episode: None,
        });
    }

    let outcome = repo::insert_batch(
        service.db(),
        Batch {
            space,
            episode: new_episode,
            memories: batch,
        },
    )
    .await
    .map_err(|error| store_error(&error))?;

    // 5. The diff. The store reports exact duplicates the same way the gate
    //    reports near ones, so both arrive as one list. This branch only runs
    //    when there was no embedding to gate on — with one, verbatim text is
    //    already a near-duplicate of itself and never reaches the transaction
    //    (issue #41) — so its `1.0` is the hash's, not a cosine's.
    let mut created = Vec::new();
    for (position, written) in outcome.memories.into_iter().enumerate() {
        let of = kept
            .get(position)
            .copied()
            .ok_or_else(|| internal("the store reported a memory the batch did not carry"))?;
        match written {
            Written::Created(id) => created.push(id.to_string()),
            Written::Duplicate(id) => duplicates.push(Duplicate {
                id: id.to_string(),
                of,
                similarity: 1.0,
            }),
        }
    }
    duplicates.sort_by_key(|duplicate| duplicate.of);

    Ok(RememberResult {
        created,
        duplicates,
        superseded: outcome.superseded.iter().map(ToString::to_string).collect(),
        episode: outcome.episode.map(|written| written.into_id().to_string()),
    })
}

impl MemoryInput {
    /// This input as a store row, or the reason it cannot be one.
    fn validated(&self, index: usize) -> Result<NewMemory, ErrorData> {
        if self.content.trim().is_empty() {
            return Err(invalid(format!("memories[{index}].content is empty")));
        }
        let mut memory = NewMemory::new(self.kind.unwrap_or(Kind::Fact), self.content.clone());
        memory.entities.clone_from(&self.entities);
        memory.tags.clone_from(&self.tags);
        memory.decay_class = self.decay_class;
        memory.supersedes = self
            .supersedes
            .as_deref()
            .map(|id| memory_id(id, &format!("memories[{index}].supersedes")))
            .transpose()?;
        memory.valid_from = self
            .valid_from
            .as_deref()
            .map(|stamp| timestamp(stamp, &format!("memories[{index}].valid_from")))
            .transpose()?;
        Ok(memory)
    }
}

impl EpisodeInput {
    /// This input as a store row — already chunked, but not yet embedded.
    fn validated(&self) -> Result<NewEpisode, ErrorData> {
        if self.content.trim().is_empty() {
            return Err(invalid("episode.content is empty"));
        }
        Ok(NewEpisode {
            content: self.content.clone(),
            occurred_at: self
                .occurred_at
                .as_deref()
                .map(|stamp| timestamp(stamp, "episode.occurred_at"))
                .transpose()?,
            session: self.session.clone(),
            chunks: chunk::chunk(&self.content)
                .into_iter()
                .map(|text| NewChunk {
                    text,
                    embedding: None,
                })
                .collect(),
        })
    }
}

/// Fill in every vector this call needs, in one pass.
///
/// BM25-only mode (`--embedder none`, `dim` 0) leaves them all unset: the rows
/// are still written and still retrievable through the fulltext index, and the
/// near-dup gate simply has nothing to measure with.
async fn embed(
    service: &AgmemService,
    memories: &mut [NewMemory],
    episode: Option<&mut NewEpisode>,
) -> Result<(), ErrorData> {
    if service.embedder().dim() == 0 {
        return Ok(());
    }
    let chunks = episode.as_deref().map_or(0, |episode| episode.chunks.len());
    let mut passages: Vec<String> = memories
        .iter()
        .map(|memory| memory.content.clone())
        .collect();
    if let Some(episode) = &episode {
        passages.extend(episode.chunks.iter().map(|chunk| chunk.text.clone()));
    }
    if passages.is_empty() {
        return Ok(());
    }

    let mut vectors = agmem_embed::embed_passages(Arc::clone(service.embedder()), passages.clone())
        .await
        .map_err(|error| internal(format!("embedding failed: {error}")))?;
    if vectors.len() != memories.len() + chunks {
        return Err(internal(
            "the embedder returned a different number of vectors than it was given",
        ));
    }

    let chunk_vectors = vectors.split_off(memories.len());
    for (memory, vector) in memories.iter_mut().zip(vectors) {
        memory.embedding = Some(vector);
    }
    if let Some(episode) = episode {
        for (chunk, vector) in episode.chunks.iter_mut().zip(chunk_vectors) {
            chunk.embedding = Some(vector);
        }
    }
    Ok(())
}

/// The space this call writes to, registered if it is a new one.
///
/// Startup registers the configured space (design §5.1 step 8); a call that
/// names another one registers it here, so `inspect` can list every space that
/// actually holds something.
async fn resolve_space(
    service: &AgmemService,
    requested: Option<&str>,
) -> Result<SpaceName, ErrorData> {
    let Some(requested) = requested else {
        return Ok(service.config().space.clone());
    };
    let space: SpaceName = requested
        .parse()
        .map_err(|error| invalid(format!("space: {error}")))?;
    if space != service.config().space {
        repo::ensure_space(service.db(), &space)
            .await
            .map_err(|error| store_error(&error))?;
    }
    Ok(space)
}

/// A memory id as sent, with or without the `memory:` table prefix agmem's own
/// output leaves off.
fn memory_id(raw: &str, field: &str) -> Result<MemoryId, ErrorData> {
    MemoryId::new(raw.strip_prefix("memory:").unwrap_or(raw))
        .map_err(|error| invalid(format!("{field}: {error}")))
}

/// An RFC3339 instant, or a message naming the field that was not one.
fn timestamp(raw: &str, field: &str) -> Result<Timestamp, ErrorData> {
    raw.parse()
        .map_err(|error| invalid(format!("{field}: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_supersedes_id_is_accepted_with_or_without_its_table() {
        let bare = "01M145SMNET1XRYA713EWAQTD3";
        assert_eq!(
            memory_id(bare, "f").expect("bare ulid").as_str(),
            memory_id(&format!("memory:{bare}"), "f")
                .expect("prefixed")
                .as_str(),
            "agmem returns bare ULIDs but the schema sketch shows memory:…; \
             both round-trip"
        );
        let error = memory_id("not-an-id", "memories[2].supersedes").expect_err("rejected");
        assert!(
            error.message.contains("memories[2].supersedes"),
            "a rejection has to name the entry that caused it: {}",
            error.message
        );
    }

    #[test]
    fn a_blank_claim_is_refused_before_it_reaches_the_store() {
        let input = MemoryInput {
            content: "   \n ".to_owned(),
            kind: None,
            entities: Vec::new(),
            tags: Vec::new(),
            decay_class: None,
            supersedes: None,
            valid_from: None,
        };
        let error = input.validated(1).expect_err("blank content is refused");
        assert!(error.message.contains("memories[1].content"), "{error:?}");
    }

    #[test]
    fn an_episode_arrives_already_chunked() {
        let input = EpisodeInput {
            content: "First para.\n\nSecond para.".to_owned(),
            occurred_at: Some("2026-08-28T09:00:00Z".to_owned()),
            session: None,
        };
        let episode = input.validated().expect("valid episode");
        assert_eq!(episode.chunks.len(), 1, "both paragraphs fit one chunk");
        assert_eq!(
            episode.occurred_at,
            Some("2026-08-28T09:00:00Z".parse().expect("timestamp"))
        );

        let bad = EpisodeInput {
            content: "text".to_owned(),
            occurred_at: Some("last tuesday".to_owned()),
            session: None,
        };
        assert!(
            bad.validated()
                .expect_err("not RFC3339")
                .message
                .contains("episode.occurred_at")
        );
    }
}
