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

use agmem_core::{DecayClass, Kind, Writer, chunk, dedup};
use agmem_store::repo::{self, Batch, NewChunk, NewEpisode, NewMemory, Written};
use jiff::Timestamp;
use rmcp::ErrorData;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::service::AgmemService;
use crate::tools::{internal, invalid, memory_id, resolve_space, store_error};

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

    /// The ids of the live memories this claim replaces. Each one is closed
    /// and stays readable and dated; only this claim is live afterwards.
    ///
    /// One id is a correction. Several is a merge — the wording worth keeping,
    /// closing every duplicate of it in the same call, which is what
    /// `consolidate`'s `near_duplicates` clusters are for. Use this rather
    /// than `forget` for anything that was once true: a closed claim keeps its
    /// history, a forgotten one does not.
    #[serde(default)]
    pub supersedes: Vec<String>,

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

    /// The claims judged already stored. Nothing was written for these.
    ///
    /// **Read `content` before treating one as a no-op.** A correction is
    /// usually worded much like the claim it corrects, so it lands here too —
    /// and then nothing has changed, the older and now-wrong claim is what is
    /// still live, and saying "already noted" would be false. If `content`
    /// says something other than what you sent, re-send yours with the id
    /// here in `supersedes`.
    pub duplicates: Vec<Duplicate>,

    /// Live claims about the same subject as something you just stored, which
    /// the new claim may contradict.
    ///
    /// These were **written** — they are not duplicates and nothing was
    /// blocked. They are here because a correction and the claim it corrects
    /// are near neighbours, and this is the only moment the id of the older
    /// one is in front of you. If one of these is now wrong, re-send the new
    /// claim with its id in `supersedes`: that closes the old one instead of
    /// leaving both live and contradicting each other. If several of them are
    /// the same claim in different words, send every id — one call closes them
    /// all. If it is merely related, ignore it.
    pub related: Vec<Related>,

    /// Ids of the memories closed by a `supersedes` in this call.
    pub superseded: Vec<String>,

    /// `supersedes` targets that were already closed when this call arrived.
    ///
    /// Nothing was rewritten for these — a supersede never rewrites another
    /// close, so each keeps its original date, reason and successor, named
    /// here. Usually this means the correction already happened (a retried
    /// call, or another session got there first); check `superseded_by`
    /// before assuming anything is still open.
    pub already_closed: Vec<AlreadyClosed>,

    /// Id of the episode, whether it was written now or already stored.
    pub episode: Option<String>,
}

/// A `supersedes` target whose close predates this call.
#[derive(Debug, Serialize, JsonSchema)]
pub struct AlreadyClosed {
    /// The id that was sent in `supersedes`.
    pub id: String,

    /// Why it was already closed: `superseded`, `forgotten` or `expired`.
    pub reason: Option<String>,

    /// The memory that replaced it, when the reason is a supersession — the
    /// live claim is there (or further down its chain), not here.
    pub superseded_by: Option<String>,
}

/// A live claim near one that was just stored.
#[derive(Debug, Serialize, JsonSchema)]
pub struct Related {
    /// The id of the older memory — what to put in `supersedes`.
    pub id: String,

    /// Which entry of the `memories` you sent this is a neighbour of,
    /// zero-based.
    pub of: usize,

    /// What the older memory says, so the contradiction can be judged without
    /// a second lookup.
    pub content: String,

    /// Cosine similarity between the two, below the duplicate threshold and
    /// above the floor for being worth mentioning at all.
    pub similarity: f64,
}

/// A claim that was already in the store.
#[derive(Debug, Serialize, JsonSchema)]
pub struct Duplicate {
    /// The id of the memory that already holds this claim.
    pub id: String,

    /// Which entry of the `memories` you sent this refers to, zero-based.
    pub of: usize,

    /// What the stored memory actually says.
    ///
    /// Compare it against what you sent: if it says something different, this
    /// is a claim you are correcting rather than one you are repeating, and it
    /// is still live until you re-send yours with `id` in `supersedes`.
    /// In BM25-only mode this is your own text, which matched exactly once
    /// case and whitespace were folded.
    pub content: String,

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
    writer: Writer,
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
    //    chunks together. agmem-embed slices the batch internally, so the
    //    shared model breathes between slices instead of being held for the
    //    call's full duration.
    embed(service, &mut new_memories, new_episode.as_mut()).await?;

    // 3. The near-dup gate: the same claim in different words is reported, not
    //    stored. A memory that carries `supersedes` skips the gate — the agent
    //    has already made the ADD/UPDATE call, and a correction is usually
    //    *close* to what it corrects.
    let gated: Vec<usize> = new_memories
        .iter()
        .enumerate()
        .filter(|(_, memory)| memory.supersedes.is_empty() && memory.embedding.is_some())
        .map(|(index, _)| index)
        .collect();
    let probes: Vec<Vec<f32>> = gated
        .iter()
        .filter_map(|&index| new_memories[index].embedding.clone())
        .collect();
    let neighbours = repo::nearest_live(service.db(), &space, &probes)
        .await
        .map_err(|error| store_error(&error))?;

    //    The same pass answers a second question (issue #38): the neighbours
    //    below the duplicate threshold but above the floor are claims the new
    //    one may be correcting, and this is the only moment their ids are in
    //    front of an agent that never called `recall`.
    let mut duplicates: Vec<Duplicate> = Vec::new();
    let mut related: Vec<Related> = Vec::new();
    let mut blocked = vec![false; new_memories.len()];
    for (index, neighbours) in gated.into_iter().zip(neighbours) {
        for neighbour in neighbours {
            if dedup::is_near_duplicate(neighbour.similarity) {
                blocked[index] = true;
                duplicates.push(Duplicate {
                    id: neighbour.id.to_string(),
                    of: index,
                    content: neighbour.content,
                    similarity: neighbour.similarity,
                });
            } else if dedup::is_correction_candidate(neighbour.similarity) {
                related.push(Related {
                    id: neighbour.id.to_string(),
                    of: index,
                    content: neighbour.content,
                    similarity: neighbour.similarity,
                });
            }
        }
    }

    // 4. One transaction for what survived. All of it duplicate and no episode
    //    means there is nothing to write at all.
    // The batch is consumed below, and a *hash* duplicate has no neighbour row
    // to quote the stored text from — but it matched the hash, so what was
    // sent is that text once case and whitespace are folded.
    let sent: Vec<String> = new_memories
        .iter()
        .map(|memory| memory.content.clone())
        .collect();
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
        // Every memory was blocked, so nothing was written and nothing can be
        // correcting anything.
        return Ok(RememberResult {
            created: Vec::new(),
            duplicates,
            related: Vec::new(),
            superseded: Vec::new(),
            already_closed: Vec::new(),
            episode: None,
        });
    }

    let outcome = repo::insert_batch(
        service.db(),
        Batch {
            space,
            episode: new_episode,
            memories: batch,
            writer,
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
                content: sent.get(of).cloned().unwrap_or_default(),
                similarity: 1.0,
            }),
        }
    }
    duplicates.sort_by_key(|duplicate| duplicate.of);

    // Nothing was written for a duplicate — by the vector gate above or by the
    // content hash just now — so it has nothing to contradict anything with,
    // and what it would have corrected is the duplicate entry's business.
    related.retain(|candidate| {
        !duplicates
            .iter()
            .any(|duplicate| duplicate.of == candidate.of)
    });
    related.sort_by_key(|candidate| candidate.of);

    Ok(RememberResult {
        created,
        duplicates,
        related,
        superseded: outcome.superseded.iter().map(ToString::to_string).collect(),
        already_closed: outcome
            .already_closed
            .into_iter()
            .map(|closed| AlreadyClosed {
                id: closed.id.to_string(),
                reason: closed.reason,
                superseded_by: closed.by.map(|id| id.to_string()),
            })
            .collect(),
        episode: outcome.episode.map(|written| written.into_id().to_string()),
    })
}

/// The most characters one claim may hold.
///
/// A memory is one distilled claim; anything longer is an artifact, and
/// artifacts belong on disk with their path stored here instead. The cap also
/// bounds what a single `remember` can put through the shared embedding model.
const MAX_MEMORY_CHARS: usize = 10_000;

/// The most characters an episode may hold — roughly seventy retrieval chunks.
///
/// Without it one multi-megabyte paste becomes thousands of chunks embedded
/// and stored in a single call, monopolising the model every other session
/// shares. Ground truth bigger than this belongs in a file, with a memory
/// pointing at it.
const MAX_EPISODE_CHARS: usize = 100_000;

impl MemoryInput {
    /// This input as a store row, or the reason it cannot be one.
    fn validated(&self, index: usize) -> Result<NewMemory, ErrorData> {
        if self.content.trim().is_empty() {
            return Err(invalid(format!("memories[{index}].content is empty")));
        }
        let length = self.content.chars().count();
        if length > MAX_MEMORY_CHARS {
            return Err(invalid(format!(
                "memories[{index}].content is {length} characters; the limit is \
                 {MAX_MEMORY_CHARS}. One memory holds one distilled claim — verbatim \
                 text goes in `episode`, and anything larger belongs on disk with its \
                 path stored here instead"
            )));
        }
        let mut memory = NewMemory::new(self.kind.unwrap_or(Kind::Fact), self.content.clone());
        memory.entities.clone_from(&self.entities);
        memory.tags.clone_from(&self.tags);
        memory.decay_class = self.decay_class;
        // A repeated id is one closure, not two: the field is the set of claims
        // this one replaces, and `UPDATE` over a list would otherwise report
        // more rows touched than there are rows.
        for (position, raw) in self.supersedes.iter().enumerate() {
            let id = memory_id(raw, &format!("memories[{index}].supersedes[{position}]"))?;
            if !memory.supersedes.contains(&id) {
                memory.supersedes.push(id);
            }
        }
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
        let length = self.content.chars().count();
        if length > MAX_EPISODE_CHARS {
            return Err(invalid(format!(
                "episode.content is {length} characters; the limit is \
                 {MAX_EPISODE_CHARS}. Store the artifact on disk and remember its \
                 path alongside the claims distilled from it"
            )));
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

    let mut vectors = agmem_embed::embed_passages(Arc::clone(service.embedder()), passages)
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

/// An RFC3339 instant, or a message naming the field that was not one.
fn timestamp(raw: &str, field: &str) -> Result<Timestamp, ErrorData> {
    raw.parse()
        .map_err(|error| invalid(format!("{field}: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blank_claim_is_refused_before_it_reaches_the_store() {
        let input = MemoryInput {
            content: "   \n ".to_owned(),
            kind: None,
            entities: Vec::new(),
            tags: Vec::new(),
            decay_class: None,
            supersedes: Vec::new(),
            valid_from: None,
        };
        let error = input.validated(1).expect_err("blank content is refused");
        assert!(error.message.contains("memories[1].content"), "{error:?}");
    }

    #[test]
    fn an_oversized_memory_is_refused_naming_the_limit() {
        let input = MemoryInput {
            content: "c".repeat(MAX_MEMORY_CHARS + 1),
            kind: None,
            entities: Vec::new(),
            tags: Vec::new(),
            decay_class: None,
            supersedes: Vec::new(),
            valid_from: None,
        };
        let error = input.validated(0).expect_err("oversized claim is refused");
        assert!(error.message.contains("memories[0].content"), "{error:?}");
        assert!(
            error.message.contains(&MAX_MEMORY_CHARS.to_string()),
            "the refusal names the limit: {error:?}"
        );
    }

    #[test]
    fn an_oversized_episode_is_refused_naming_the_limit() {
        let input = EpisodeInput {
            content: "e".repeat(MAX_EPISODE_CHARS + 1),
            occurred_at: None,
            session: None,
        };
        let error = input.validated().expect_err("oversized episode is refused");
        assert!(error.message.contains("episode.content"), "{error:?}");
        assert!(
            error.message.contains(&MAX_EPISODE_CHARS.to_string()),
            "the refusal names the limit: {error:?}"
        );
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
