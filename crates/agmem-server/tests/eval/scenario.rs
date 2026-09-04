//! The eval fixtures and their replay: scripted sessions with known facts,
//! corrections and distractors, seeded through the real `remember` path.
//!
//! Fixtures name memories by a scenario-local `label`; ids only exist once a
//! seed has been stored, so everything that needs one — `supersedes`, a
//! probe's `relevant` set, a gate case's `original` — goes through the
//! label→id map the replay builds from each `remember`'s own answer. Seeding
//! asserts its own outcome before anything is scored: a seed that gets gated
//! or refused is a broken fixture, not a quality number.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use agmem_embed::Embedder;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::documents::Document;
use crate::harness::{Harness, ids};

/// One scripted session: what goes in, and what each metric may ask about it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub name: String,
    /// Prose for the fixture reader; the harness never looks at it.
    #[serde(default)]
    #[expect(dead_code, reason = "deserialized so `deny_unknown_fields` permits it")]
    pub about: String,
    pub seeds: Vec<Seed>,
    #[serde(default)]
    pub probes: Vec<Probe>,
    #[serde(default)]
    pub abstain: Vec<AbstainCase>,
    #[serde(default)]
    pub temporal: Vec<TemporalCheck>,
    #[serde(default)]
    pub timeline: Vec<TimelineCheck>,
    #[serde(default)]
    pub gate: Vec<GateCase>,
    #[serde(default)]
    pub context: Vec<ContextCase>,
}

/// One memory the session stores, plus the episode it may arrive with.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Seed {
    pub label: String,
    pub content: String,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub entities: Vec<String>,
    #[serde(default)]
    pub valid_from: Option<String>,
    /// Labels of earlier seeds this one corrects.
    #[serde(default)]
    pub supersedes: Vec<String>,
    /// Verbatim session text stored alongside this claim.
    #[serde(default)]
    pub episode: Option<String>,
    /// Labels of earlier seeds this one was reflected out of (issue #85).
    /// Non-empty routes the seed through `reflect` instead of `remember`,
    /// which is the only way a `summary` can be written.
    #[serde(default)]
    pub derived_from: Vec<String>,
}

/// One retrieval question with its human-labelled answer set.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Probe {
    pub query: String,
    pub k: u16,
    pub relevant: Vec<String>,
}

/// One question this store cannot answer: the honest page is empty
/// (issue #77). No `relevant` set — the labelled ground truth is that
/// nothing seeded qualifies.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AbstainCase {
    pub query: String,
    pub k: u16,
}

/// One time-aware retrieval check (issue #78): a query with a soft
/// `since`/`until`/`changed_since` window, judged on which claim tops the
/// page. The window rescores rather than filters, so this measures ranking
/// under temporal intent — `as_of`'s hard cut is the timeline section's job.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemporalCheck {
    pub query: String,
    #[serde(default)]
    pub since: Option<String>,
    #[serde(default)]
    pub until: Option<String>,
    #[serde(default)]
    pub changed_since: Option<String>,
    /// A past window over a live read cannot surface closed claims at all;
    /// checks about what *was* true set this, exactly as a caller would.
    #[serde(default)]
    pub include_invalidated: bool,
    /// The label the top claim hit must carry.
    pub expect_top: String,
}

/// One supersession check: what should answer, live or as of an instant.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimelineCheck {
    pub query: String,
    #[serde(default)]
    pub as_of: Option<String>,
    /// The label the top hit must carry.
    pub expect_top: String,
    /// When set, the top hit must be a closed claim pointing at this label.
    #[serde(default)]
    pub superseded_by: Option<String>,
    /// Labels that must not appear anywhere in the hits.
    #[serde(default)]
    pub absent: Vec<String>,
}

/// One write the duplicate gate is judged on, after the seeds are in.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GateCase {
    pub candidate: String,
    pub expect: GateExpect,
    /// The seed a gated candidate must be reported as a duplicate of.
    #[serde(default)]
    pub original: Option<String>,
}

/// The human-labelled ground truth for a gate case — what *should* happen,
/// which the recorded baseline may honestly miss.
#[derive(Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum GateExpect {
    Gated,
    Stored,
}

/// One `context` call and what its block must and must not carry; the
/// structural checklist (headings, budget, ids, no superseded or verbatim
/// text) applies to every case on top of these.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCase {
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub budget_chars: Option<u32>,
    /// Labels whose content the block must include.
    #[serde(default)]
    pub must_contain: Vec<String>,
    /// Raw text — an episode marker, not a label — the block must not leak.
    #[serde(default)]
    pub must_not_contain_text: Vec<String>,
}

impl Scenario {
    /// The labels of seeds that some later seed supersedes.
    pub fn superseded_labels(&self) -> Vec<&str> {
        self.seeds
            .iter()
            .flat_map(|seed| seed.supersedes.iter().map(String::as_str))
            .collect()
    }
}

/// Every committed scenario, in file order.
pub fn all() -> Vec<Scenario> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/eval/scenarios");
    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .expect("read the scenario dir")
        .map(|entry| entry.expect("dir entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "no scenarios in {}", dir.display());
    paths
        .iter()
        .map(|path| {
            let raw = std::fs::read_to_string(path).expect("read scenario");
            serde_json::from_str(&raw)
                .unwrap_or_else(|error| panic!("{} does not parse: {error}", path.display()))
        })
        .collect()
}

/// A server with one scenario's seeds stored, and the ids they landed as.
pub struct Seeded {
    pub agmem: Harness,
    id_of: HashMap<String, String>,
}

impl Seeded {
    /// The id a label landed as; a label no seed carries is a fixture bug.
    pub fn id(&self, label: &str) -> &str {
        self.id_of
            .get(label)
            .unwrap_or_else(|| panic!("no seed labelled {label:?}"))
    }

    pub async fn shutdown(self) {
        self.agmem.shutdown().await;
    }
}

/// Replays a scenario's seeds in order on a fresh store.
///
/// Every scorer starts from its own call to this rather than sharing one
/// store: `recall` reinforces what it returns, so two probes on one store
/// would couple — the second measured against strengths the first shifted.
pub async fn seed(scenario: &Scenario, embedder: Arc<dyn Embedder>) -> Seeded {
    seed_with(scenario, embedder, &[]).await
}

/// [`seed`], with a document corpus stored first (issue #137). Documents go
/// in before the scenario's seeds so their `occurred_at` order mirrors a real
/// store, where the plan predates the claims distilled from it; nothing in
/// the scenario refers to them, and a probe's labelled-relevant set never
/// names a chunk, so the corpus is pure competition for the page.
pub async fn seed_with(
    scenario: &Scenario,
    embedder: Arc<dyn Embedder>,
    documents: &[Document],
) -> Seeded {
    let agmem = Harness::start(embedder).await;
    for document in documents {
        let answer = agmem
            .remember(json!({
                "memories": [],
                "episode": {
                    "content": document.content,
                    "title": document.title,
                    "doc_kind": document.doc_kind,
                    "tags": document.tags,
                    "mime": "text/markdown"
                }
            }))
            .await;
        assert!(
            answer["episode"].is_string(),
            "document {:?} must land before {:?} is scored: {answer}",
            document.title,
            scenario.name
        );
    }
    let mut id_of: HashMap<String, String> = HashMap::new();
    for seed in &scenario.seeds {
        if !seed.derived_from.is_empty() {
            let id = reflect_seed(&agmem, scenario, seed, &id_of).await;
            id_of.insert(seed.label.clone(), id);
            continue;
        }
        let mut memory = Map::new();
        memory.insert("content".into(), json!(seed.content));
        if let Some(kind) = &seed.kind {
            memory.insert("kind".into(), json!(kind));
        }
        if !seed.tags.is_empty() {
            memory.insert("tags".into(), json!(seed.tags));
        }
        if !seed.entities.is_empty() {
            memory.insert("entities".into(), json!(seed.entities));
        }
        if let Some(valid_from) = &seed.valid_from {
            memory.insert("valid_from".into(), json!(valid_from));
        }
        if !seed.supersedes.is_empty() {
            let targets: Vec<&String> = seed
                .supersedes
                .iter()
                .map(|label| {
                    id_of
                        .get(label)
                        .unwrap_or_else(|| panic!("{} supersedes unseeded {label:?}", seed.label))
                })
                .collect();
            memory.insert("supersedes".into(), json!(targets));
        }
        let mut arguments = json!({ "memories": [Value::Object(memory)] });
        if let Some(episode) = &seed.episode {
            arguments["episode"] = json!({ "content": episode });
        }
        let answer = agmem.remember(arguments).await;
        let created = ids(&answer["created"]);
        assert_eq!(
            created.len(),
            1,
            "seed {:?} of {:?} must land before anything is scored: {answer}",
            seed.label,
            scenario.name
        );
        id_of.insert(seed.label.clone(), created[0].to_owned());
    }
    Seeded { agmem, id_of }
}

/// One seed stored through `reflect`, citing earlier seeds by label.
async fn reflect_seed(
    agmem: &Harness,
    scenario: &Scenario,
    seed: &Seed,
    id_of: &HashMap<String, String>,
) -> String {
    assert!(
        seed.episode.is_none(),
        "{}: `reflect` takes no episode; store it on a cited seed instead",
        seed.label
    );
    assert!(
        seed.supersedes.is_empty() && seed.valid_from.is_none(),
        "{}: the reflect route does not carry these fields — wire them here \
         when a fixture first needs them, rather than dropping them silently",
        seed.label
    );
    let cited: Vec<&String> = seed
        .derived_from
        .iter()
        .map(|label| {
            id_of
                .get(label)
                .unwrap_or_else(|| panic!("{} cites unseeded {label:?}", seed.label))
        })
        .collect();
    let mut arguments = json!({ "insight": seed.content, "derived_from": cited });
    if let Some(kind) = &seed.kind {
        arguments["kind"] = json!(kind);
    }
    if !seed.tags.is_empty() {
        arguments["tags"] = json!(seed.tags);
    }
    if !seed.entities.is_empty() {
        arguments["entities"] = json!(seed.entities);
    }
    let answer = agmem.reflect(arguments).await;
    assert_eq!(
        answer["created"],
        Value::Bool(true),
        "seed {:?} of {:?} must land before anything is scored: {answer}",
        seed.label,
        scenario.name
    );
    answer["id"]
        .as_str()
        .expect("reflect answers with the stored id")
        .to_owned()
}
