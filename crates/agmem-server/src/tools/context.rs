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

use std::collections::{HashMap, HashSet};

use agmem_core::scoring::{self, Signals};
use agmem_core::{Derivation, Kind, MemoryId, MemoryRecord, SpaceName};
use agmem_store::repo::{self, Filters, Hit as StoreHit, Lookup, Search};
use jiff::Timestamp;
use rmcp::ErrorData;
use rmcp::model::{CallToolResult, ContentBlock};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::service::AgmemService;
use crate::tools::{self, LESSONS_PER_TAG, embed_query, invalid, store_error};

/// How much room a call that does not say gets, in characters.
const DEFAULT_BUDGET_CHARS: u32 = 6_000;

/// The smallest budget worth assembling. Below it the block cannot hold its
/// title and a claim, and an empty answer reads like an empty store — so a
/// budget this small is refused rather than served misleadingly.
const MIN_BUDGET_CHARS: u32 = 200;

/// How many claims the Relevant section contributes when a query aims it.
const RELEVANT_K: usize = 10;

/// How many claims the Relevant section contributes with no query (issue
/// #152). Unaimed, the section is a recency-and-strength list rather than an
/// answer, and ten of those filled the default budget before Lessons got a
/// line on a real store.
const RELEVANT_UNAIMED_K: usize = 5;

/// How many lessons the Lessons section contributes (design §3.2).
const LESSONS_K: usize = 5;

/// How much of the budget the sections above Lessons may not spend, so a
/// store's hard-won how-tos reach the session even when Relevant could fill
/// the block on its own (issue #152). A third of the default budget: three
/// lessons of the length a real store writes them (~600 characters). Capped at
/// a third of any budget so a small block still leads with its instructions,
/// and never held back for room the section has nothing to put in.
const LESSONS_RESERVE_CHARS: usize = 2_000;

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
    /// Characters the sections before this one may not spend, so that filling
    /// in priority order cannot starve it. Bounded by what the section could
    /// actually use, so an empty section reserves nothing.
    reserve: usize,
}

impl Section {
    /// The room this section can hold back from the ones above it: its
    /// reserve, or less when its heading and every entry would not fill it.
    fn held(&self) -> usize {
        if self.reserve == 0 || self.entries.is_empty() {
            return 0;
        }
        let demand = format!("\n\n{}", self.heading).chars().count()
            + self
                .entries
                .iter()
                .map(|entry| entry.line().chars().count())
                .sum::<usize>();
        self.reserve.min(demand)
    }
}

/// One line of the block: a claim, and the id that leads back to it.
struct Entry {
    id: MemoryId,
    content: String,
    /// The memories a `summary` stands in for (issue #85) — what a roll-up
    /// render marks as shown once the summary itself is. Empty for anything
    /// that is not a summary with memory citations.
    covers: Vec<MemoryId>,
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
    // working for, what today is about, what past sessions paid to learn. The
    // last of those holds a reserve (issue #152): priority decides who fills
    // first, not who gets to exist.
    let sections = [
        Section {
            heading: HEADING_INSTRUCTIONS,
            reserve: 0,
            entries: lookup_section(
                service,
                &spaces,
                Filters {
                    kinds: vec![Kind::Instruction],
                    ..Filters::default()
                },
                pool,
                None,
                now,
            )
            .await?,
        },
        Section {
            heading: HEADING_PROFILE,
            reserve: 0,
            entries: lookup_section(
                service,
                &spaces,
                Filters {
                    kinds: vec![Kind::Fact],
                    tags: vec![IDENTITY_TAG.to_owned()],
                    ..Filters::default()
                },
                pool,
                None,
                now,
            )
            .await?,
        },
        Section {
            heading: HEADING_RELEVANT,
            reserve: 0,
            entries: match &query {
                Some(text) => search_section(service, &spaces, text, now).await?,
                // No query is not "nothing to say" — it is the general case,
                // and what a session wants then is the strongest facts that
                // have held up (design §3.2), with the summaries that stand
                // in for whole groups of them (issue #85) alongside. Fewer of
                // them than an aimed search returns: unaimed, the list is
                // orientation, not an answer (issue #152).
                None => {
                    lookup_section(
                        service,
                        &spaces,
                        Filters {
                            kinds: vec![Kind::Fact, Kind::Summary],
                            ..Filters::default()
                        },
                        RELEVANT_UNAIMED_K,
                        None,
                        now,
                    )
                    .await?
                }
            },
        },
        Section {
            heading: HEADING_LESSONS,
            reserve: lessons_reserve(budget),
            entries: lookup_section(
                service,
                &spaces,
                Filters {
                    kinds: vec![Kind::Lesson],
                    ..Filters::default()
                },
                LESSONS_K,
                Some(LESSONS_PER_TAG),
                now,
            )
            .await?,
        },
    ];

    let markdown = assemble(&spaces, &sections, budget);
    Ok(CallToolResult::success(vec![ContentBlock::text(markdown)]))
}

/// What Lessons holds back from the sections above it at this budget.
fn lessons_reserve(budget: usize) -> usize {
    LESSONS_RESERVE_CHARS.min(budget / 3)
}

/// The block, fitted to its budget in up to three fills.
///
/// The first fill shows everything that fits. Only when it dropped entries do
/// summaries earn their keep (issue #85): the roll-up fill lets each emitted
/// summary absorb the claims it covers, so the budget buys breadth instead of
/// repeating what the digest already says — and if *that* fill fits whole,
/// the block is complete and carries no note. Otherwise the fill runs once
/// more with room reserved for [`TRIMMED`], which is only known to be needed
/// once a fill has run.
fn assemble(spaces: &[SpaceName], sections: &[Section], budget: usize) -> String {
    let (block, dropped) = render(spaces, sections, budget, false);
    if dropped == 0 {
        return block;
    }
    let (rolled, dropped) = render(spaces, sections, budget, true);
    if dropped == 0 {
        return rolled;
    }
    let reserved = budget.saturating_sub(TRIMMED.chars().count());
    let (block, _) = render(spaces, sections, reserved, true);
    format!("{block}{TRIMMED}")
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
    per_tag: Option<usize>,
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
        per_tag,
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
        None,
    ))
}

/// Rank candidates the way `recall` does, and keep the best `keep`.
///
/// `per_tag` bounds how many of the kept entries may share a tag: records over
/// the quota are deferred behind everything under it before the cut, so a
/// flooded tag yields slots it would otherwise monopolise without ever
/// costing the section entries it had room for.
fn rank(
    candidates: impl IntoIterator<Item = (MemoryRecord, Signals)>,
    keep: usize,
    per_tag: Option<usize>,
) -> Vec<Entry> {
    let ranked = scoring::rank(candidates)
        .into_iter()
        .map(|(memory, _)| memory);
    let ordered = match per_tag {
        Some(cap) => cap_by_tag(ranked.collect(), cap),
        None => ranked.collect(),
    };
    ordered.into_iter().take(keep).map(Entry::from).collect()
}

impl From<MemoryRecord> for Entry {
    fn from(memory: MemoryRecord) -> Self {
        let covers = if memory.kind == Kind::Summary {
            memory
                .derived_from
                .into_iter()
                .filter_map(|cited| match cited {
                    Derivation::Memory(id) => Some(id),
                    Derivation::Episode(_) => None,
                })
                .collect()
        } else {
            Vec::new()
        };
        Self {
            id: memory.id,
            content: memory.content,
            covers,
        }
    }
}

/// Defer, never drop: re-order ranked records so no tag holds more than `cap`
/// of the head of the list (issue #82, the same shape as `recall`'s
/// per-source occupancy cap).
///
/// A record is deferred once *any* of its tags is at quota — the only honest
/// reading for multi-tag records — and an admitted record counts against every
/// tag it carries. Untagged records are never deferred: the cap exists for
/// playbook-style tag floods, and a record no tag claims is not part of one.
/// Order is otherwise preserved, deferred records included, so a `keep` larger
/// than the survivors still reaches them strongest-first.
fn cap_by_tag(records: Vec<MemoryRecord>, cap: usize) -> Vec<MemoryRecord> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut kept = Vec::with_capacity(records.len());
    let mut deferred = Vec::new();
    for record in records {
        let at_quota = record
            .tags
            .iter()
            .any(|tag| counts.get(tag).is_some_and(|held| *held >= cap));
        if at_quota {
            deferred.push(record);
            continue;
        }
        for tag in &record.tags {
            *counts.entry(tag.clone()).or_insert(0) += 1;
        }
        kept.push(record);
    }
    kept.append(&mut deferred);
    kept
}

/// The block, and how many entries the budget cost it.
///
/// The title is the frame and is always written; everything after it is
/// budgeted. A heading is charged to the first entry under it that fits, so a
/// section whose entries all went over budget leaves no empty heading behind —
/// and a section the store had nothing for never appears at all.
///
/// `roll_up` is the budget-pressure mode (issue #85): an emitted summary
/// marks everything it covers as already shown, so its children neither cost
/// budget nor count as dropped — the digest is standing in for them, which is
/// what it was written to do, and each one stays one `inspect` away.
///
/// A section fills against the budget less what every later section holds
/// back (`Section::held`), so filling in priority order can no longer starve a
/// section that has something to say (issue #152). What a reserved section
/// then leaves unused is not lost — it simply was not needed. The first
/// section never yields to a reserve: what the agent must obey outranks what
/// would help it, and at the budget floor the two cannot both fit.
fn render(
    spaces: &[SpaceName],
    sections: &[Section],
    budget: usize,
    roll_up: bool,
) -> (String, usize) {
    let names: Vec<String> = spaces.iter().map(ToString::to_string).collect();
    let title = format!("# Memory context (spaces: {})", names.join(" + "));
    let title_chars = title.chars().count();
    let mut used = title_chars;
    let mut text = title;
    let mut dropped = 0;
    let mut seen: HashSet<&MemoryId> = HashSet::new();

    for (index, section) in sections.iter().enumerate() {
        let held: usize = if index == 0 {
            0
        } else {
            sections[index + 1..].iter().map(Section::held).sum()
        };
        let budget = budget.saturating_sub(held);
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
            if roll_up {
                seen.extend(&entry.covers);
            }
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
    use agmem_core::Source;

    use super::*;

    fn id(last: char) -> MemoryId {
        MemoryId::new(format!("01M145SMNH1V44GYMHB5KG5MX{last}")).expect("ulid")
    }

    fn entry(last: char, content: &str) -> Entry {
        Entry {
            id: id(last),
            content: content.to_owned(),
            covers: Vec::new(),
        }
    }

    fn summary(last: char, content: &str, covers: &[char]) -> Entry {
        Entry {
            id: id(last),
            content: content.to_owned(),
            covers: covers.iter().map(|last| id(*last)).collect(),
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
                reserve: 0,
                entries: vec![entry('A', "Run the linter before committing.")],
            },
            Section {
                heading: HEADING_PROFILE,
                reserve: 0,
                entries: Vec::new(),
            },
        ];
        let (block, dropped) = render(&spaces(), &sections, 6_000, false);

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
        let (block, dropped) = render(&spaces(), &[], 6_000, false);
        assert_eq!(dropped, 0);
        assert!(block.ends_with(NOTHING), "{block}");
    }

    #[test]
    fn the_budget_drops_whole_entries_and_keeps_the_first_section() {
        let sections = [
            Section {
                heading: HEADING_INSTRUCTIONS,
                reserve: 0,
                entries: vec![entry('A', "Short instruction.")],
            },
            Section {
                heading: HEADING_LESSONS,
                reserve: 0,
                entries: vec![entry('B', &"long ".repeat(60))],
            },
        ];
        let (block, dropped) = render(&spaces(), &sections, 200, false);

        assert_eq!(dropped, 1, "{block}");
        assert!(block.chars().count() <= 200, "{block}");
        assert!(block.contains("Short instruction."), "{block}");
        assert!(!block.contains(HEADING_LESSONS), "{block}");
    }

    #[test]
    fn a_reserve_keeps_a_later_section_from_being_starved() {
        // Ten facts of ~90 chars would take every character of a 700 budget on
        // their own; the reserve holds room back for the lessons behind them.
        let sections = |reserve| {
            [
                Section {
                    heading: HEADING_INSTRUCTIONS,
                    reserve: 0,
                    entries: vec![entry('Z', "Obey the budget.")],
                },
                Section {
                    heading: HEADING_RELEVANT,
                    reserve: 0,
                    entries: ('A'..='J')
                        .map(|last| entry(last, &format!("fact {last} {}", "words ".repeat(8))))
                        .collect(),
                },
                Section {
                    heading: HEADING_LESSONS,
                    reserve,
                    entries: vec![
                        entry('K', "lesson one: warm the cache first"),
                        entry('L', "lesson two: pin the toolchain"),
                        entry('M', "lesson three: never trust a green cold build"),
                    ],
                },
            ]
        };

        let (starved, _) = render(&spaces(), &sections(0), 700, false);
        assert!(!starved.contains(HEADING_LESSONS), "{starved}");

        let (fed, _) = render(&spaces(), &sections(220), 700, false);
        assert!(fed.chars().count() <= 700, "{fed}");
        for lesson in ["lesson one", "lesson two", "lesson three"] {
            assert!(fed.contains(lesson), "{lesson} is missing from {fed}");
        }
        assert!(
            fed.contains("fact A"),
            "the section above still fills first: {fed}"
        );

        // A reserve is bounded by demand: an empty section holds nothing back
        // from the ones above it.
        let empty = [
            Section {
                heading: HEADING_INSTRUCTIONS,
                reserve: 0,
                entries: vec![entry('Z', "Obey the budget.")],
            },
            Section {
                heading: HEADING_RELEVANT,
                reserve: 0,
                entries: vec![entry('A', "the only fact")],
            },
            Section {
                heading: HEADING_LESSONS,
                reserve: 5_000,
                entries: Vec::new(),
            },
        ];
        let (block, dropped) = render(&spaces(), &empty, 250, false);
        assert_eq!(dropped, 0, "{block}");
        assert!(block.contains("the only fact"), "{block}");

        // And the first section never yields: a reserve that would evict the
        // instruction is what gives, not the instruction.
        let squeezed = [
            Section {
                heading: HEADING_INSTRUCTIONS,
                reserve: 0,
                entries: vec![entry('Z', &"obey ".repeat(20))],
            },
            Section {
                heading: HEADING_LESSONS,
                reserve: 5_000,
                entries: vec![entry('K', &"learn ".repeat(20))],
            },
        ];
        let (block, _) = render(&spaces(), &squeezed, 200, false);
        assert!(block.contains("obey obey"), "{block}");
    }

    #[test]
    fn a_later_entry_still_fits_after_one_too_long_for_the_budget() {
        let sections = [Section {
            heading: HEADING_PROFILE,
            reserve: 0,
            entries: vec![
                entry('A', &"long ".repeat(60)),
                entry('B', "The user works in Rust."),
            ],
        }];
        let (block, dropped) = render(&spaces(), &sections, 200, false);

        assert_eq!(dropped, 1);
        assert!(block.contains("The user works in Rust."), "{block}");
    }

    #[test]
    fn a_claim_is_never_shown_twice() {
        let sections = [
            Section {
                heading: HEADING_PROFILE,
                reserve: 0,
                entries: vec![entry('A', "The user prefers Rust.")],
            },
            Section {
                heading: HEADING_RELEVANT,
                reserve: 0,
                entries: vec![entry('A', "The user prefers Rust.")],
            },
        ];
        let (block, dropped) = render(&spaces(), &sections, 6_000, false);

        assert_eq!(dropped, 0);
        assert_eq!(
            block.matches("The user prefers Rust.").count(),
            1,
            "{block}"
        );
        assert!(!block.contains(HEADING_RELEVANT), "{block}");
    }

    /// The one section every roll-up test renders: a summary covering A and
    /// B, its two children, and one claim outside its reach.
    fn covered_sections() -> [Section; 1] {
        [Section {
            heading: HEADING_RELEVANT,
            reserve: 0,
            entries: vec![
                summary('S', "Refresh was hardened end to end.", &['A', 'B']),
                entry('A', "The refresh timeout was raised to thirty seconds."),
                entry('B', "Login requests retry once on refresh failure."),
                entry('C', "Deploys happen Tuesdays."),
            ],
        }]
    }

    #[test]
    fn a_roll_up_absorbs_what_an_emitted_summary_covers() {
        let sections = covered_sections();
        let (plain, _) = render(&spaces(), &sections, 6_000, false);
        assert!(
            plain.contains("thirty seconds") && plain.contains("retry once"),
            "with room, the children show alongside the summary: {plain}"
        );

        let (rolled, dropped) = render(&spaces(), &sections, 6_000, true);
        assert_eq!(dropped, 0, "absorbed children are covered, not dropped");
        assert!(
            !rolled.contains("thirty seconds") && !rolled.contains("retry once"),
            "{rolled}"
        );
        assert!(
            rolled.contains("Deploys happen Tuesdays."),
            "a claim the summary does not cover still shows: {rolled}"
        );
    }

    #[test]
    fn a_roll_up_that_fits_whole_carries_no_trimmed_note() {
        // 180 drops both children on the plain fill (the summary and the last
        // claim fit; each child would overflow), so assembly rolls up — and
        // then everything left fits, which must read as complete.
        let block = assemble(&spaces(), &covered_sections(), 180);
        assert!(
            block.contains("Refresh was hardened end to end."),
            "{block}"
        );
        assert!(block.contains("Deploys happen Tuesdays."), "{block}");
        assert!(!block.contains("thirty seconds"), "{block}");
        assert!(!block.contains("Trimmed"), "{block}");
    }

    #[test]
    fn a_roll_up_still_over_budget_says_so() {
        // 130 holds the summary but not the uncovered claim even after the
        // roll-up, so the note is earned.
        let block = assemble(&spaces(), &covered_sections(), 130);
        assert!(block.ends_with(TRIMMED), "{block}");
    }

    fn lesson(last: char, tags: &[&str]) -> MemoryRecord {
        MemoryRecord {
            id: id(last),
            space: SpaceName::user(),
            kind: Kind::Lesson,
            content: format!("lesson {last}"),
            content_hash: format!("hash-{last}"),
            entities: vec![],
            tags: tags.iter().map(|tag| (*tag).to_owned()).collect(),
            embedding: None,
            decay_class: Kind::Lesson.default_decay_class(),
            strength: 1.0,
            last_accessed: Timestamp::UNIX_EPOCH,
            access_count: 0,
            valid_from: Timestamp::UNIX_EPOCH,
            invalid_at: None,
            invalid_reason: None,
            supersedes: Vec::new(),
            superseded_by: None,
            source: Source::Agent,
            writer: None,
            novelty: None,
            derived_from: Vec::new(),
            created_at: Timestamp::UNIX_EPOCH,
        }
    }

    fn order(records: &[MemoryRecord]) -> String {
        records
            .iter()
            .map(|record| record.content.trim_start_matches("lesson ").to_owned())
            .collect()
    }

    #[test]
    fn a_flooded_tag_yields_its_slots_past_the_cap() {
        let capped = cap_by_tag(
            vec![
                lesson('A', &["role:architect"]),
                lesson('B', &["role:architect"]),
                lesson('C', &["role:architect"]),
                lesson('D', &["role:architect"]),
                lesson('E', &["ops"]),
                lesson('F', &[]),
            ],
            3,
        );
        // D goes behind everything under quota; nothing is dropped.
        assert_eq!(order(&capped), "ABCEFD");
    }

    #[test]
    fn a_record_is_deferred_once_any_of_its_tags_is_at_quota() {
        let capped = cap_by_tag(
            vec![
                lesson('A', &["role:architect"]),
                lesson('B', &["role:architect", "ops"]),
                lesson('C', &["ops", "ci"]),
                lesson('D', &["ci"]),
            ],
            1,
        );
        // B shares role:architect with A, D shares ci with C.
        assert_eq!(order(&capped), "ACBD");
    }

    #[test]
    fn untagged_records_are_never_deferred() {
        let capped = cap_by_tag(
            vec![lesson('A', &[]), lesson('B', &[]), lesson('C', &[])],
            1,
        );
        assert_eq!(order(&capped), "ABC");
    }

    #[test]
    fn a_multi_line_claim_stays_one_bullet() {
        let sections = [Section {
            heading: HEADING_LESSONS,
            reserve: 0,
            entries: vec![entry('A', "  Builds fail\non a cold cache.\n")],
        }];
        let (block, _) = render(&spaces(), &sections, 6_000, false);

        assert!(
            block.contains("- Builds fail on a cold cache. `"),
            "{block}"
        );
        assert_eq!(block.lines().count(), 4, "{block}");
    }
}
