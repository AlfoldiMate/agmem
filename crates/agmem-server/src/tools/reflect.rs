//! `reflect` — the insight verb (design §3.1, issue #26).
//!
//! A reflection is just another memory row. What makes it one is the evidence
//! it carries: the ids of the claims and the verbatim text it was drawn from,
//! stored on the row as `derived_from` and rendered by `inspect`. That is the
//! Generative Agents pattern with the same line drawn as everywhere else here
//! — the *agent* does the reflecting, and agmem stores the conclusion together
//! with what it was built on, so a later session can check the evidence
//! instead of taking the conclusion on faith.
//!
//! Everything else about the write is `remember`'s: one embedding pass, the
//! near-duplicate gate, the same correction candidates from the same probe.
//! An insight worth storing twice is still one claim.

use std::sync::Arc;

use agmem_core::{Derivation, Kind, MemoryId, SpaceName, Writer, dedup};
use agmem_store::repo::{self, Batch, NewMemory};
use rmcp::ErrorData;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::service::AgmemService;
use crate::tools::{internal, invalid, memory_id, resolve_space, store_error};

/// One `reflect` call: the insight, and what it was drawn from.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReflectParams {
    /// Where to store this. Defaults to the space this server was started
    /// with; `user` is the reserved space for memory that follows the person
    /// across every project.
    #[serde(default)]
    pub space: Option<String>,

    /// The insight itself: one atomic, self-contained statement in the third
    /// person, understandable with no conversation around it. What you
    /// concluded, not the reasoning that got you there.
    pub insight: String,

    /// The ids this insight was drawn from — memory ids, episode ids, or
    /// both, exactly as `recall`, `remember` or `context` handed them to you.
    /// At least one; an insight with nothing behind it belongs in `remember`.
    pub derived_from: Vec<String>,

    /// What the insight is; defaults to `lesson`.
    #[serde(default)]
    pub kind: Option<Kind>,

    /// The subjects it is about ("user", "project-x"), for filtered recall.
    #[serde(default)]
    pub entities: Vec<String>,

    /// Free labels. `identity` marks a fact that belongs in every session's
    /// profile.
    #[serde(default)]
    pub tags: Vec<String>,

    /// The ids of the live claims this insight replaces. Each one is closed
    /// and stays readable and dated; only the insight is live afterwards.
    ///
    /// Several at once is a merge: one cited conclusion standing in for every
    /// uncited wording of it, closed in the same call rather than forgotten.
    #[serde(default)]
    pub supersedes: Vec<String>,
}

/// What the call stored.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ReflectResult {
    /// The insight's id — newly minted, or the id of the claim that already
    /// said this.
    pub id: String,

    /// Whether the row was written now.
    ///
    /// `false` means an equivalent insight was already stored and nothing
    /// changed. Read `content`: if it says something other than what you sent,
    /// yours is a correction rather than a repetition, and it is still unsaid
    /// until you send it again with `id` in `supersedes`.
    pub created: bool,

    /// What is stored under `id` — your wording when it was written, the
    /// existing claim's when it was not.
    pub content: String,

    /// The evidence recorded with the insight, as refs `inspect` takes as they
    /// stand.
    ///
    /// Empty when `created` is false: nothing was written, and the claim
    /// already under `id` keeps whatever evidence it was written with.
    pub derived_from: Vec<String>,

    /// Live claims near the insight, which it may be correcting.
    ///
    /// These were **not** blocked and are not duplicates — they are here
    /// because a correction and the claim it corrects are near neighbours, and
    /// this is the only moment the older one's id is in front of you. If one
    /// of them is what your insight replaces, send the insight again with its
    /// id in `supersedes` — several ids if several of them are the same claim
    /// worded differently. If it is merely related, ignore it.
    pub related: Vec<Related>,

    /// The ids of the claims closed by a `supersedes` in this call.
    pub superseded: Vec<String>,

    /// `supersedes` targets that were already closed when this call arrived —
    /// skipped and reported, never rewritten, exactly as `remember` does.
    pub already_closed: Vec<super::remember::AlreadyClosed>,

    /// What to do about a write that did not happen, when there is something
    /// worth doing.
    ///
    /// It appears when the insight was judged a duplicate of a claim that
    /// **carries no evidence of its own** — the conclusion is already stored,
    /// its provenance is not, and nothing here rewrites a stored claim. A
    /// no-op that leaves an uncited conclusion standing is not the same
    /// outcome as one that leaves a cited one standing, and only this says so.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// A live claim near the insight that was just stored.
#[derive(Debug, Serialize, JsonSchema)]
pub struct Related {
    /// The id of the older claim — what to put in `supersedes`.
    pub id: String,

    /// What it says, so the two can be judged without a second lookup.
    pub content: String,

    /// Cosine similarity between the two, below the duplicate threshold and
    /// above the floor for being worth mentioning at all.
    pub similarity: f64,
}

/// Store one insight with its citations (design §5.2, issue #26).
///
/// # Errors
/// [`ErrorData`] with `INVALID_PARAMS` for anything the caller can fix — a
/// blank insight, no citations at all, a citation that names nothing in the
/// spaces searched, a `supersedes` id this space does not hold — and
/// `INTERNAL_ERROR` for a failing embedder or store.
pub async fn run(
    service: &AgmemService,
    params: ReflectParams,
    writer: Writer,
) -> Result<ReflectResult, ErrorData> {
    let ReflectParams {
        space,
        insight,
        derived_from,
        kind,
        entities,
        tags,
        supersedes,
    } = params;
    let space = resolve_space(service, space.as_deref()).await?;
    if insight.trim().is_empty() {
        return Err(invalid("insight is empty"));
    }

    // 1. The citations, before anything is embedded or written. Resolving them
    //    is also what validates them: a ULID says nothing about its table, so
    //    the store is what turns one into `memory:` or `episode:`.
    let cited = resolve(service, &space, &derived_from).await?;

    let mut memory = NewMemory::new(kind.unwrap_or(Kind::Lesson), insight.clone());
    memory.entities = entities;
    memory.tags = tags;
    memory.derived_from = cited.clone();
    // A repeated id is one closure, not two — the field is the set of claims
    // this insight replaces, the same way `derived_from` is a set of citations.
    for (position, raw) in supersedes.iter().enumerate() {
        let id = memory_id(raw, &format!("supersedes[{position}]"))?;
        if !memory.supersedes.contains(&id) {
            memory.supersedes.push(id);
        }
    }

    // 2. One vector for the one claim, unless the embedder has none to give.
    if service.embedder().dim() > 0 {
        let mut vectors =
            agmem_embed::embed_passages(Arc::clone(service.embedder()), vec![insight.clone()])
                .await
                .map_err(|error| internal(format!("embedding failed: {error}")))?;
        memory.embedding = vectors.pop();
    }

    // 3. The near-dup gate, exactly as `remember` runs it: the same insight in
    //    different words is reported rather than stored, and the rest of the
    //    probe's band is handed back as claims this one may be correcting. A
    //    call carrying `supersedes` skips it — the judgement is already made,
    //    and a correction reads much like what it corrects.
    let mut related = Vec::new();
    if let (true, Some(probe)) = (memory.supersedes.is_empty(), &memory.embedding) {
        let neighbours = repo::nearest_live(service.db(), &space, std::slice::from_ref(probe))
            .await
            .map_err(|error| store_error(&error))?;
        for neighbour in neighbours.into_iter().flatten() {
            if dedup::is_near_duplicate(neighbour.similarity) {
                let note = uncited(service, &space, &neighbour.id).await;
                return Ok(ReflectResult {
                    id: neighbour.id.to_string(),
                    created: false,
                    content: neighbour.content,
                    derived_from: Vec::new(),
                    related: Vec::new(),
                    superseded: Vec::new(),
                    already_closed: Vec::new(),
                    note,
                });
            }
            if dedup::is_correction_candidate(neighbour.similarity) {
                related.push(Related {
                    id: neighbour.id.to_string(),
                    content: neighbour.content,
                    similarity: neighbour.similarity,
                });
            }
        }
    }

    // 4. One row, one transaction.
    let space_written = space.clone();
    let outcome = repo::insert_batch(
        service.db(),
        Batch {
            space,
            episode: None,
            memories: vec![memory],
            writer,
        },
    )
    .await
    .map_err(|error| store_error(&error))?;
    let written = outcome
        .memories
        .into_iter()
        .next()
        .ok_or_else(|| internal("the store reported no memory for a batch of one"))?;

    // An exact-hash duplicate never reached the vector gate, which means there
    // was no embedding to gate with — so nothing was written, and the citations
    // this call carried were not recorded against the row that already exists.
    let created = written.is_created();
    let id = written.into_id();
    let note = if created {
        None
    } else {
        uncited(service, &space_written, &id).await
    };
    Ok(ReflectResult {
        id: id.to_string(),
        created,
        content: insight,
        derived_from: if created {
            cited.iter().map(ToString::to_string).collect()
        } else {
            Vec::new()
        },
        related: if created { related } else { Vec::new() },
        superseded: outcome.superseded.iter().map(ToString::to_string).collect(),
        already_closed: outcome
            .already_closed
            .into_iter()
            .map(|closed| super::remember::AlreadyClosed {
                id: closed.id.to_string(),
                reason: closed.reason,
                superseded_by: closed.by.map(|id| id.to_string()),
            })
            .collect(),
        note,
    })
}

/// The move left open when an insight was not written, or `None` when there is
/// none.
///
/// The question is only ever about the claim that blocked it: if that claim
/// already carries citations, a duplicate really is a no-op and there is
/// nothing to say. If it carries none, the conclusion is stored *without its
/// provenance* and the only way to attach them is to supersede it — which is
/// what the answer has to name, because `created: false` on its own reads as
/// "already handled" and the evidence quietly goes nowhere (measured 2 runs
/// out of 3 at #26).
///
/// A store that will not answer is not worth failing the write over: the row
/// was just reported by the store, so a miss here means something odd
/// happened, and the caller still needs its id back.
async fn uncited(service: &AgmemService, space: &SpaceName, id: &MemoryId) -> Option<String> {
    let chain = repo::history_chain(service.db(), space, id).await.ok()?;
    let stored = chain.into_iter().find(|link| link.id == *id)?;
    if !stored.derived_from.is_empty() {
        return None;
    }
    Some(format!(
        "Nothing was written: memory:{id} already says this, and it carries no \
         evidence of its own. Send this insight again with that id in \
         `supersedes` to store the same conclusion with its citations attached \
         — a stored claim is never rewritten, so superseding it is the only way \
         to give it provenance."
    ))
}

/// Every citation as the store resolved it, in the order they were sent.
///
/// Ids are looked for in the space being written to *and* in the reserved
/// `user` space, which is the pair every read defaults to: an insight about
/// this project is often drawn partly from what is known about the person, and
/// refusing that citation would teach the agent to leave the evidence out
/// rather than to file it elsewhere.
///
/// A repeated id is stored once — the row is a set of citations, not a tally.
async fn resolve(
    service: &AgmemService,
    space: &SpaceName,
    requested: &[String],
) -> Result<Vec<Derivation>, ErrorData> {
    if requested.is_empty() {
        return Err(invalid(
            "derived_from is empty: pass the ids this insight was drawn from, \
             or use `remember` for a claim with nothing behind it",
        ));
    }

    // What the caller wrote, split into the bare ULID to look up and the table
    // it claimed. A prefix is checked rather than trusted: `memory:<id>` for an
    // id that names an episode is a mistake worth naming, not one to correct
    // silently.
    let asked: Vec<(String, Option<&str>)> = requested
        .iter()
        .map(|raw| {
            let raw = raw.trim();
            match raw.split_once(':') {
                Some((table @ ("memory" | "episode"), id)) => Ok((id.to_owned(), Some(table))),
                None => Ok((raw.to_owned(), None)),
                Some(_) => Err(grammar(raw)),
            }
        })
        .collect::<Result<_, ErrorData>>()?;

    let ids: Vec<String> = asked.iter().map(|(id, _)| id.clone()).collect();
    let mut spaces = vec![space.clone(), SpaceName::user()];
    spaces.dedup();
    let found = repo::locate(service.db(), &spaces, &ids)
        .await
        .map_err(|error| store_error(&error))?;

    let mut cited: Vec<Derivation> = Vec::with_capacity(found.len());
    for ((id, claimed), resolved) in asked.iter().zip(found) {
        let names = spaces
            .iter()
            .map(SpaceName::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        let Some(resolved) = resolved else {
            return Err(invalid(format!(
                "derived_from: no memory or episode {id} in {names}; cite what \
                 `recall`, `remember` or `context` handed you"
            )));
        };
        if let Some(claimed) = claimed {
            let actual = match resolved {
                Derivation::Memory(_) => "memory",
                Derivation::Episode(_) => "episode",
            };
            if *claimed != actual {
                return Err(invalid(format!(
                    "derived_from: {claimed}:{id} does not exist; that id names a {actual}"
                )));
            }
        }
        if !cited.contains(&resolved) {
            cited.push(resolved);
        }
    }
    Ok(cited)
}

/// The citation grammar, as something to act on rather than guess at.
fn grammar(raw: &str) -> ErrorData {
    invalid(format!(
        "derived_from takes ids as `memory:<id>`, `episode:<id>`, or bare; got {raw:?}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_citation_with_the_wrong_table_is_refused_by_the_grammar() {
        let error = grammar("chunk:01M145SMNET1XRYA713EWAQTD3");
        assert!(
            error.message.contains("derived_from takes ids"),
            "{}",
            error.message
        );
    }
}
