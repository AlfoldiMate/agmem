//! `context` — everything worth knowing before the first move (design §3.2).
//!
//! Spectron's best idea with the LLM taken out: one markdown block, four fixed
//! sections in a fixed order, assembled from four reads and a formatter. There
//! is no synthesis anywhere in here, which is what makes the same store produce
//! the same block.
//!
//! Two things separate it from a `recall` with no query. It is *budgeted* —
//! sections fill in priority order until `budget_chars` runs out, and an entry
//! that does not fit is dropped whole rather than cut, because half a claim is
//! worse than no claim. And it does *not* reinforce what it returns: `context`
//! is called on a schedule rather than because something was needed, so
//! counting it as use would flatten every memory's decay curve to permanent
//! within a handful of sessions and retire the whole idea of decay.

use std::collections::HashSet;

use agmem_core::scoring::{self, Signals};
use agmem_core::{Kind, MemoryId, MemoryRecord, SpaceName};
use agmem_store::repo::{self, Filters, Hit as StoreHit, Lookup, Search};
use jiff::Timestamp;
use rmcp::ErrorData;
use rmcp::model::{CallToolResult, ContentBlock};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::service::AgmemService;
use crate::tools::{self, embed_query, invalid, store_error};

/// How much room a call that does not say gets, in characters.
const DEFAULT_BUDGET_CHARS: u32 = 6_000;

/// The smallest budget worth assembling. Below it the block cannot hold its
/// title and a claim, and an empty answer reads like an empty store — so a
/// budget this small is refused rather than served misleadingly.
const MIN_BUDGET_CHARS: u32 = 200;

/// How many claims the Relevant section contributes.
const RELEVANT_K: usize = 10;

/// How many lessons the Lessons section contributes (design §3.2).
const LESSONS_K: usize = 5;

/// The tag that makes a fact part of the profile (design §3.2).
const IDENTITY_TAG: &str = "identity";

const HEADING_INSTRUCTIONS: &str = "## Instructions";
const HEADING_PROFILE: &str = "## Profile";
const HEADING_RELEVANT: &str = "## Relevant";
const HEADING_LESSONS: &str = "## Lessons";

/// What the block says when the budget cost it entries.
///
/// It carries no count on purpose: the note has to be inside the budget, and
/// reserving room for it changes how many entries fit, which would change the
/// count, which would change the note's length. A fixed string settles in one
/// extra pass instead of iterating to a fixed point.
const TRIMMED: &str = "\n\n_Trimmed to fit `budget_chars`; `recall` reaches what is missing._";

/// What the block says when the spaces are empty.
const NOTHING: &str = "\n\n_Nothing stored for these spaces yet._";

/// One `context` call: what to aim it at, and how much room it has.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ContextParams {
    /// What this session is about, in words. It aims the Relevant section at
    /// the work in front of you; the other three sections do not change with
    /// it. Leave it out for a general orientation.
    #[serde(default)]
    pub query: Option<String>,

    /// Where to look: `current` for this project, `user` for the person,
    /// `all` for every space, or a space name. Defaults to `current` and
    /// `user` together.
    #[serde(default)]
    pub space: Option<String>,

    /// How many characters the block may take, 6000 by default. Sections fill
    /// in priority order until it runs out; whole entries are dropped, never
    /// cut mid-sentence.
    #[serde(default)]
    pub budget_chars: Option<u32>,
}

/// One section of the block: a heading, and what goes under it in order.
struct Section {
    heading: &'static str,
    entries: Vec<Entry>,
}

/// One line of the block: a claim, and the id that leads back to it.
struct Entry {
    id: MemoryId,
    content: String,
}

impl Entry {
    /// The bullet as it appears, id included.
    ///
    /// The id is 26 characters of the budget per entry and worth it: an agent
    /// that reads a stale claim here can hand the id straight to `inspect` or
    /// to `remember`'s `supersedes` without a `recall` in between.
    ///
    /// Whitespace is collapsed so a multi-line claim stays one bullet. That is
    /// a rendering choice, not an edit — the stored text is untouched, and
    /// `recall` and `inspect` still return it verbatim.
    fn line(&self) -> String {
        let content: Vec<&str> = self.content.split_whitespace().collect();
        format!("\n- {} `{}`", content.join(" "), self.id)
    }
}

/// Assemble the session-start block (design §3.2).
///
/// # Errors
/// [`ErrorData`] with `INVALID_PARAMS` for a bad space name or a budget below
/// the floor, and `INTERNAL_ERROR` for a failing embedder or store.
pub async fn run(
    service: &AgmemService,
    params: ContextParams,
) -> Result<CallToolResult, ErrorData> {
    let ContextParams {
        query,
        space,
        budget_chars,
    } = params;

    let spaces = tools::spaces(service, space.as_deref()).await?;
    let budget = resolve_budget(budget_chars)?;
    let query = query.filter(|text| !text.trim().is_empty());
    let now = Timestamp::now();
    let pool = usize::from(service.config().pool);

    // The order is the priority order: what the agent must obey, who it is
    // working for, what today is about, what past sessions paid to learn.
    let sections = [
        Section {
            heading: HEADING_INSTRUCTIONS,
            entries: lookup_section(
                service,
                &spaces,
                Filters {
                    kinds: vec![Kind::Instruction],
                    ..Filters::default()
                },
                pool,
                now,
            )
            .await?,
        },
        Section {
            heading: HEADING_PROFILE,
            entries: lookup_section(
                service,
                &spaces,
                Filters {
                    kinds: vec![Kind::Fact],
                    tags: vec![IDENTITY_TAG.to_owned()],
                    ..Filters::default()
                },
                pool,
                now,
            )
            .await?,
        },
        Section {
            heading: HEADING_RELEVANT,
            entries: match &query {
                Some(text) => search_section(service, &spaces, text, now).await?,
                // No query is not "nothing to say" — it is the general case,
                // and what a session wants then is the strongest facts that
                // have held up (design §3.2).
                None => {
                    lookup_section(
                        service,
                        &spaces,
                        Filters {
                            kinds: vec![Kind::Fact],
                            ..Filters::default()
                        },
                        RELEVANT_K,
                        now,
                    )
                    .await?
                }
            },
        },
        Section {
            heading: HEADING_LESSONS,
            entries: lookup_section(
                service,
                &spaces,
                Filters {
                    kinds: vec![Kind::Lesson],
                    ..Filters::default()
                },
                LESSONS_K,
                now,
            )
            .await?,
        },
    ];

    let (block, dropped) = render(&spaces, &sections, budget);
    let markdown = if dropped == 0 {
        block
    } else {
        // Room for the note is only known to be needed once the first fill has
        // run, so the fill runs again with that room reserved.
        let reserved = budget.saturating_sub(TRIMMED.chars().count());
        let (block, _) = render(&spaces, &sections, reserved);
        format!("{block}{TRIMMED}")
    };
    Ok(CallToolResult::success(vec![ContentBlock::text(markdown)]))
}

/// One tier-1 section: an indexed lookup, ranked and cut to `keep`.
///
/// The ranking is `recall`'s, on a pool with no retrieval score — so the order
/// is retention and importance, which is exactly design §3.2's "by strength"
/// and "by strength·recency": strength is what flattens the decay curve, and
/// retention is what that curve is worth today.
async fn lookup_section(
    service: &AgmemService,
    spaces: &[SpaceName],
    filters: Filters,
    keep: usize,
    now: Timestamp,
) -> Result<Vec<Entry>, ErrorData> {
    let mut lookup = Lookup::new(spaces.to_vec());
    lookup.filters = filters;
    lookup.limit = usize::from(service.config().pool);
    let records = repo::direct_lookup(service.db(), &lookup)
        .await
        .map_err(|error| store_error(&error))?;
    Ok(rank(
        records.into_iter().map(|memory| {
            let signals = Signals::for_memory(0.0, &memory, now);
            (memory, signals)
        }),
        keep,
    ))
}

/// The Relevant section when the call carried a query: `recall`'s hybrid
/// search, minus the verbatim half.
///
/// Episode chunks are left out deliberately. A chunk runs to ~1500 characters
/// (`core::chunk`), so one of them eats a quarter of the default budget and
/// takes the Lessons section down with it. The block is a briefing; `recall`
/// is still the way to the verbatim text behind any line of it.
async fn search_section(
    service: &AgmemService,
    spaces: &[SpaceName],
    text: &str,
    now: Timestamp,
) -> Result<Vec<Entry>, ErrorData> {
    let mut search = Search::new(spaces.to_vec());
    search.vector = embed_query(service, text).await?;
    search.text = Some(text.to_owned());
    search.pool = usize::from(service.config().pool);
    search.episodes = false;
    search.fusion = service.config().fusion;
    let candidates = repo::search_hybrid(service.db(), &search)
        .await
        .map_err(|error| store_error(&error))?;
    Ok(rank(
        candidates
            .into_iter()
            .filter_map(|candidate| match candidate.hit {
                StoreHit::Memory(memory) => {
                    let memory = *memory;
                    let signals = Signals::for_memory(candidate.rrf, &memory, now);
                    Some((memory, signals))
                }
                // `episodes` is off above, so this is the store changing its
                // mind rather than a case with an answer.
                StoreHit::Chunk(_) => None,
            }),
        RELEVANT_K,
    ))
}

/// Rank candidates the way `recall` does, and keep the best `keep`.
fn rank(candidates: impl IntoIterator<Item = (MemoryRecord, Signals)>, keep: usize) -> Vec<Entry> {
    scoring::rank(candidates)
        .into_iter()
        .take(keep)
        .map(|(memory, _)| Entry {
            id: memory.id,
            content: memory.content,
        })
        .collect()
}

/// The block, and how many entries the budget cost it.
///
/// The title is the frame and is always written; everything after it is
/// budgeted. A heading is charged to the first entry under it that fits, so a
/// section whose entries all went over budget leaves no empty heading behind —
/// and a section the store had nothing for never appears at all.
fn render(spaces: &[SpaceName], sections: &[Section], budget: usize) -> (String, usize) {
    let names: Vec<String> = spaces.iter().map(ToString::to_string).collect();
    let title = format!("# Memory context (spaces: {})", names.join(" + "));
    let title_chars = title.chars().count();
    let mut used = title_chars;
    let mut text = title;
    let mut dropped = 0;
    let mut seen: HashSet<&MemoryId> = HashSet::new();

    for section in sections {
        let header = format!("\n\n{}", section.heading);
        let mut header_cost = header.chars().count();
        for entry in &section.entries {
            // A claim already shown higher up is not shown twice: the same
            // fact is often both part of the profile and the best match for
            // the query, and a repeat costs budget without adding anything.
            if !seen.insert(&entry.id) {
                continue;
            }
            let line = entry.line();
            let cost = header_cost + line.chars().count();
            // A long entry is skipped rather than ending the section — the
            // next one may fit, and a fuller block is worth losing strict
            // rank order at the tail.
            if used + cost > budget {
                dropped += 1;
                continue;
            }
            if header_cost > 0 {
                text.push_str(&header);
                header_cost = 0;
            }
            text.push_str(&line);
            used += cost;
        }
    }

    // "Nothing landed" and "nothing is stored" are different answers, and only
    // the second one is this note's; the first is what `TRIMMED` says.
    if used == title_chars && dropped == 0 && used + NOTHING.chars().count() <= budget {
        text.push_str(NOTHING);
    }
    (text, dropped)
}

/// How much room the block gets, refused rather than silently raised.
///
/// A budget under the floor cannot hold a heading and one claim, so it would
/// come back as a bare title — indistinguishable from an empty store, which is
/// the one thing a session-start block must never get wrong.
fn resolve_budget(requested: Option<u32>) -> Result<usize, ErrorData> {
    let budget = requested.unwrap_or(DEFAULT_BUDGET_CHARS);
    if budget < MIN_BUDGET_CHARS {
        return Err(invalid(format!(
            "budget_chars must be at least {MIN_BUDGET_CHARS}"
        )));
    }
    Ok(usize::try_from(budget).unwrap_or(usize::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(last: char) -> MemoryId {
        MemoryId::new(format!("01M145SMNH1V44GYMHB5KG5MX{last}")).expect("ulid")
    }

    fn entry(last: char, content: &str) -> Entry {
        Entry {
            id: id(last),
            content: content.to_owned(),
        }
    }

    fn spaces() -> Vec<SpaceName> {
        vec!["default".parse().expect("slug"), SpaceName::user()]
    }

    #[test]
    fn budget_below_the_floor_is_refused_and_the_default_applies() {
        assert_eq!(
            resolve_budget(None).expect("default"),
            DEFAULT_BUDGET_CHARS as usize
        );
        assert_eq!(resolve_budget(Some(200)).expect("floor"), 200);
        assert!(
            resolve_budget(Some(10))
                .expect_err("under the floor")
                .message
                .contains("budget_chars")
        );
    }

    #[test]
    fn an_empty_section_leaves_no_heading_behind() {
        let sections = [
            Section {
                heading: HEADING_INSTRUCTIONS,
                entries: vec![entry('A', "Run the linter before committing.")],
            },
            Section {
                heading: HEADING_PROFILE,
                entries: Vec::new(),
            },
        ];
        let (block, dropped) = render(&spaces(), &sections, 6_000);

        assert_eq!(dropped, 0);
        assert!(block.starts_with("# Memory context (spaces: default + user)"));
        assert!(block.contains(HEADING_INSTRUCTIONS), "{block}");
        assert!(!block.contains(HEADING_PROFILE), "{block}");
        assert!(
            block.contains("- Run the linter before committing. `01M145SMNH1V44GYMHB5KG5MXA`"),
            "{block}"
        );
    }

    #[test]
    fn an_empty_store_says_so_rather_than_answering_with_a_bare_title() {
        let (block, dropped) = render(&spaces(), &[], 6_000);
        assert_eq!(dropped, 0);
        assert!(block.ends_with(NOTHING), "{block}");
    }

    #[test]
    fn the_budget_drops_whole_entries_and_keeps_the_first_section() {
        let sections = [
            Section {
                heading: HEADING_INSTRUCTIONS,
                entries: vec![entry('A', "Short instruction.")],
            },
            Section {
                heading: HEADING_LESSONS,
                entries: vec![entry('B', &"long ".repeat(60))],
            },
        ];
        let (block, dropped) = render(&spaces(), &sections, 200);

        assert_eq!(dropped, 1, "{block}");
        assert!(block.chars().count() <= 200, "{block}");
        assert!(block.contains("Short instruction."), "{block}");
        assert!(!block.contains(HEADING_LESSONS), "{block}");
    }

    #[test]
    fn a_later_entry_still_fits_after_one_too_long_for_the_budget() {
        let sections = [Section {
            heading: HEADING_PROFILE,
            entries: vec![
                entry('A', &"long ".repeat(60)),
                entry('B', "The user works in Rust."),
            ],
        }];
        let (block, dropped) = render(&spaces(), &sections, 200);

        assert_eq!(dropped, 1);
        assert!(block.contains("The user works in Rust."), "{block}");
    }

    #[test]
    fn a_claim_is_never_shown_twice() {
        let sections = [
            Section {
                heading: HEADING_PROFILE,
                entries: vec![entry('A', "The user prefers Rust.")],
            },
            Section {
                heading: HEADING_RELEVANT,
                entries: vec![entry('A', "The user prefers Rust.")],
            },
        ];
        let (block, dropped) = render(&spaces(), &sections, 6_000);

        assert_eq!(dropped, 0);
        assert_eq!(
            block.matches("The user prefers Rust.").count(),
            1,
            "{block}"
        );
        assert!(!block.contains(HEADING_RELEVANT), "{block}");
    }

    #[test]
    fn a_multi_line_claim_stays_one_bullet() {
        let sections = [Section {
            heading: HEADING_LESSONS,
            entries: vec![entry('A', "  Builds fail\non a cold cache.\n")],
        }];
        let (block, _) = render(&spaces(), &sections, 6_000);

        assert!(
            block.contains("- Builds fail on a cold cache. `"),
            "{block}"
        );
        assert_eq!(block.lines().count(), 4, "{block}");
    }
}
