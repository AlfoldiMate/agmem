//! The five scorers and the scorecard they fill (issue #32).
//!
//! Every number here is read off a tool call's *returned or stored* result —
//! never off "the call happened". The scorecard is all integers except MRR:
//! counts survive a JSON round trip exactly, so the baseline in the doc can
//! be asserted with plain equality, and a regression names the probe that
//! moved instead of nudging a float.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use agmem_embed::Embedder;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::harness::{headings, hits};
use crate::scenario::{self, GateExpect, Scenario};

/// Everything the eval measures, keyed by scenario.
#[derive(Serialize, Deserialize, PartialEq, Debug)]
pub struct Scorecard {
    pub scenarios: BTreeMap<String, ScenarioScore>,
}

#[derive(Serialize, Deserialize, PartialEq, Debug)]
pub struct ScenarioScore {
    pub retrieval: Retrieval,
    pub timeline: Ratio,
    pub gate: Gate,
    pub context: Ratio,
    pub staleness: Staleness,
    pub abstention: Abstention,
    /// Time-aware ranking (issue #78): soft-window checks, one pass/fail
    /// each, judged on the top claim exactly as `timeline`'s live rule is.
    pub temporal: Ratio,
}

/// How much of the labelled-relevant set the probes brought back, and how
/// early. `found`/`expected` is recall@k over all probes; `returned` is every
/// hit the probes handed back — the precision denominator, without which a
/// trim's cost is measured and its benefit is not; `mrr` is the mean
/// reciprocal rank of each probe's first relevant hit, rounded to four
/// decimals so the doc round-trips to the same f64; `ndcg5` (issue #137)
/// is the mean nDCG@5 over the page as returned, chunk hits included at
/// gain 0, so anything that displaces a relevant claim from the top of the
/// page costs here even when `found` is unmoved.
#[derive(Serialize, Deserialize, PartialEq, Debug)]
pub struct Retrieval {
    pub found: u32,
    pub expected: u32,
    pub returned: u32,
    pub mrr: f64,
    pub ndcg5: f64,
}

/// The cut-off nDCG is scored at.
pub const NDCG_AT: usize = 5;

/// Normalised discounted cumulative gain at [`NDCG_AT`] for one page:
/// binary gain, `log2(rank + 1)` discount, ideal DCG from as many relevant
/// items as could have fit. A page with nothing to find scores 0.
pub fn ndcg(ranked: &[&str], relevant: &HashSet<&str>) -> f64 {
    if relevant.is_empty() {
        return 0.0;
    }
    let discount = |rank: usize| 1.0 / ((rank + 2) as f64).log2();
    let dcg: f64 = ranked
        .iter()
        .take(NDCG_AT)
        .enumerate()
        .filter(|(_, id)| relevant.contains(*id))
        .map(|(rank, _)| discount(rank))
        .sum();
    let ideal: f64 = (0..relevant.len().min(NDCG_AT)).map(discount).sum();
    dcg / ideal
}

#[derive(Serialize, Deserialize, PartialEq, Debug)]
pub struct Ratio {
    pub passed: u32,
    pub total: u32,
}

/// The duplicate gate against human-labelled ground truth. `correct` counts
/// cases where the gate agreed with the label *and*, for gated ones, named
/// the right original; the three failure columns say how the rest went
/// wrong, because a gate that never fires and one that always fires can post
/// the same accuracy.
#[derive(Serialize, Deserialize, PartialEq, Debug)]
pub struct Gate {
    pub correct: u32,
    pub total: u32,
    /// Labelled a fresh claim, but the gate blocked it.
    pub false_gates: u32,
    /// Labelled a restatement, but it was stored.
    pub missed: u32,
    /// Gated as labelled, but reported the wrong original.
    pub wrong_original: u32,
}

/// Supersession honesty over live pages (FAMA-style, issue #79). `pages` is
/// how many live recalls the scenario scripts; `stale_hits` how many of
/// their hits were claims corrected before the query ran. Zero is the only
/// honest number — anything above it means a superseded claim reached a
/// caller who never asked for history.
#[derive(Serialize, Deserialize, PartialEq, Debug)]
pub struct Staleness {
    pub stale_hits: u32,
    pub pages: u32,
}

/// Honest empty pages (issue #77). `fired`/`expected` is over the abstain
/// cases — queries whose labelled ground truth is that nothing seeded
/// answers them; `false_abstentions`/`pages` replays every probe and counts
/// the ones that wrongly came back empty, because an abstainer that fires on
/// everything posts a perfect `fired` score.
#[derive(Serialize, Deserialize, PartialEq, Debug)]
pub struct Abstention {
    pub fired: u32,
    pub expected: u32,
    pub false_abstentions: u32,
    pub pages: u32,
}

/// Scores every scenario with the given embedder.
pub async fn scorecard(scenarios: &[Scenario], embedder: Arc<dyn Embedder>) -> Scorecard {
    let mut scores = BTreeMap::new();
    for scenario in scenarios {
        scores.insert(
            scenario.name.clone(),
            ScenarioScore {
                retrieval: retrieval(scenario, embedder.clone()).await,
                timeline: timeline(scenario, embedder.clone()).await,
                gate: gate(scenario, embedder.clone()).await,
                context: context(scenario, embedder.clone()).await,
                staleness: staleness(scenario, embedder.clone()).await,
                abstention: abstention(scenario, embedder.clone()).await,
                temporal: temporal(scenario, embedder.clone()).await,
            },
        );
    }
    Scorecard { scenarios: scores }
}

/// The id of every hit, in rank order.
fn hit_ids(found: &Value) -> Vec<&str> {
    hits(found)
        .iter()
        .map(|hit| hit["id"].as_str().expect("an id"))
        .collect()
}

/// recall@k and MRR over the probes, each on a fresh store.
pub async fn retrieval(scenario: &Scenario, embedder: Arc<dyn Embedder>) -> Retrieval {
    let mut found = 0;
    let mut expected = 0;
    let mut returned_total = 0;
    let mut reciprocal_sum = 0.0;
    let mut ndcg_sum = 0.0;
    for probe in &scenario.probes {
        let seeded = scenario::seed(scenario, embedder.clone()).await;
        let answer = seeded
            .agmem
            .recall(json!({ "query": probe.query, "k": probe.k }))
            .await;
        let relevant: HashSet<&str> = probe
            .relevant
            .iter()
            .map(|label| seeded.id(label))
            .collect();
        let returned = hit_ids(&answer);
        found += returned.iter().filter(|id| relevant.contains(*id)).count() as u32;
        expected += relevant.len().min(usize::from(probe.k)) as u32;
        returned_total += returned.len() as u32;
        if let Some(rank) = returned.iter().position(|id| relevant.contains(id)) {
            reciprocal_sum += 1.0 / (rank + 1) as f64;
        }
        ndcg_sum += ndcg(&returned, &relevant);
        seeded.shutdown().await;
    }
    let probes = scenario.probes.len();
    let mean = |sum: f64| {
        if probes == 0 {
            0.0
        } else {
            round4(sum / probes as f64)
        }
    };
    Retrieval {
        found,
        expected,
        returned: returned_total,
        mrr: mean(reciprocal_sum),
        ndcg5: mean(ndcg_sum),
    }
}

/// Abstention over the labelled unanswerables, and its false-positive check
/// over every probe: an empty page where the fixture labels hits relevant is
/// a wrong abstention wherever the mechanism that emptied it lives.
pub async fn abstention(scenario: &Scenario, embedder: Arc<dyn Embedder>) -> Abstention {
    let mut fired = 0;
    for case in &scenario.abstain {
        let seeded = scenario::seed(scenario, embedder.clone()).await;
        let answer = seeded
            .agmem
            .recall(json!({ "query": case.query, "k": case.k }))
            .await;
        fired += u32::from(hit_ids(&answer).is_empty());
        seeded.shutdown().await;
    }
    let mut false_abstentions = 0;
    for probe in &scenario.probes {
        let seeded = scenario::seed(scenario, embedder.clone()).await;
        let answer = seeded
            .agmem
            .recall(json!({ "query": probe.query, "k": probe.k }))
            .await;
        false_abstentions += u32::from(hit_ids(&answer).is_empty());
        seeded.shutdown().await;
    }
    Abstention {
        fired,
        expected: scenario.abstain.len() as u32,
        false_abstentions,
        pages: scenario.probes.len() as u32,
    }
}

fn round4(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

/// Time-aware ranking (issue #78): each check recalls with its soft window
/// and passes when the expected claim tops the page. The window rescores
/// rather than filters, so what this measures is whether temporal intent
/// can reorder a page the plain query would rank differently — the
/// `WEIGHT_TEMPORAL = 0` counterfactual in `docs/eval/quality.md` is the
/// proof of which checks the term itself carries.
pub async fn temporal(scenario: &Scenario, embedder: Arc<dyn Embedder>) -> Ratio {
    let mut passed = 0;
    for check in &scenario.temporal {
        let seeded = scenario::seed(scenario, embedder.clone()).await;
        let mut arguments = json!({ "query": check.query, "k": 10 });
        for (field, value) in [
            ("since", &check.since),
            ("until", &check.until),
            ("changed_since", &check.changed_since),
        ] {
            if let Some(stamp) = value {
                arguments[field] = json!(stamp);
            }
        }
        if check.include_invalidated {
            arguments["include_invalidated"] = json!(true);
        }
        let answer = seeded.agmem.recall(arguments).await;
        // Judged on the first *claim*, as timeline's live rule is: an
        // episode slice outranking it is a ranking fact with its own column.
        let top = hits(&answer)
            .iter()
            .find(|hit| hit["kind"].as_str() != Some("episode"))
            .cloned()
            .unwrap_or(Value::Null);
        passed += u32::from(top["id"].as_str() == Some(seeded.id(check.expect_top.as_str())));
        seeded.shutdown().await;
    }
    Ratio {
        passed,
        total: scenario.temporal.len() as u32,
    }
}

/// Supersession correctness: what answers live, what answers as-of, and how
/// a closed claim is annotated. One pass/fail per timeline entry — every
/// condition it states must hold.
pub async fn timeline(scenario: &Scenario, embedder: Arc<dyn Embedder>) -> Ratio {
    let mut passed = 0;
    for check in &scenario.timeline {
        let seeded = scenario::seed(scenario, embedder.clone()).await;
        let mut arguments = json!({ "query": check.query, "k": 10 });
        if let Some(as_of) = &check.as_of {
            arguments["as_of"] = json!(as_of);
        }
        let answer = seeded.agmem.recall(arguments).await;

        // For a live check, `expect_top` is judged on the first *claim*: an
        // episode slice outranking it is a ranking fact, and ranking has its
        // own column. An as-of check judges the raw top instead — chunks are
        // dated since schema v4, and a chunk surfacing for an instant before
        // its episode happened is exactly the failure this metric watches.
        let top = if check.as_of.is_some() {
            hits(&answer).first().cloned().unwrap_or(Value::Null)
        } else {
            hits(&answer)
                .iter()
                .find(|hit| hit["kind"].as_str() != Some("episode"))
                .cloned()
                .unwrap_or(Value::Null)
        };
        let mut ok = top["id"].as_str() == Some(seeded.id(check.expect_top.as_str()));
        if let Some(successor) = &check.superseded_by {
            ok = ok
                && top["invalid_reason"].as_str() == Some("superseded")
                && top["superseded_by"].as_str() == Some(seeded.id(successor));
        }
        for label in &check.absent {
            ok = ok && !hit_ids(&answer).contains(&seeded.id(label));
        }
        passed += u32::from(ok);
        seeded.shutdown().await;
    }
    Ratio {
        passed,
        total: scenario.timeline.len() as u32,
    }
}

/// Replays every live page the scenario scripts — each probe at its own `k`,
/// each timeline check without `as_of` at the timeline's k of 10 — and
/// counts hits that are claims some later seed superseded. Ground truth is
/// the fixture's `supersedes` graph, not the hit's own annotation: a closed
/// claim that leaks unannotated still counts.
pub async fn staleness(scenario: &Scenario, embedder: Arc<dyn Embedder>) -> Staleness {
    let mut pages: Vec<(&str, u16)> = scenario
        .probes
        .iter()
        .map(|probe| (probe.query.as_str(), probe.k))
        .collect();
    pages.extend(
        scenario
            .timeline
            .iter()
            .filter(|check| check.as_of.is_none())
            .map(|check| (check.query.as_str(), 10)),
    );
    let mut stale_hits = 0;
    for (query, k) in &pages {
        let seeded = scenario::seed(scenario, embedder.clone()).await;
        let answer = seeded.agmem.recall(json!({ "query": query, "k": k })).await;
        let superseded: HashSet<&str> = scenario
            .superseded_labels()
            .iter()
            .map(|label| seeded.id(label))
            .collect();
        stale_hits += hit_ids(&answer)
            .iter()
            .filter(|id| superseded.contains(*id))
            .count() as u32;
        seeded.shutdown().await;
    }
    Staleness {
        stale_hits,
        pages: pages.len() as u32,
    }
}

/// The duplicate gate, one fresh store per candidate so cases never see each
/// other's writes. Gated means the answer's `duplicates` names it and
/// nothing was created.
pub async fn gate(scenario: &Scenario, embedder: Arc<dyn Embedder>) -> Gate {
    let mut score = Gate {
        correct: 0,
        total: scenario.gate.len() as u32,
        false_gates: 0,
        missed: 0,
        wrong_original: 0,
    };
    for case in &scenario.gate {
        let seeded = scenario::seed(scenario, embedder.clone()).await;
        let answer = seeded
            .agmem
            .remember(json!({ "memories": [{ "content": case.candidate }] }))
            .await;
        let duplicates = answer["duplicates"].as_array().expect("array");
        let was_gated = !duplicates.is_empty();
        match (case.expect, was_gated) {
            (GateExpect::Gated, true) => {
                let original = case
                    .original
                    .as_ref()
                    .expect("a gated case names its original");
                if duplicates[0]["id"].as_str() == Some(seeded.id(original)) {
                    score.correct += 1;
                } else {
                    score.wrong_original += 1;
                }
            }
            (GateExpect::Gated, false) => score.missed += 1,
            (GateExpect::Stored, false) => score.correct += 1,
            (GateExpect::Stored, true) => score.false_gates += 1,
        }
        seeded.shutdown().await;
    }
    score
}

/// The fixed order `context` lays sections out in.
const SECTION_ORDER: [&str; 4] = ["## Instructions", "## Profile", "## Relevant", "## Lessons"];

/// What a `context` call may spend when the case does not say.
const DEFAULT_BUDGET_CHARS: usize = 6_000;

/// The context-block checklist: one pass/fail unit per structural property
/// and per fixture expectation, so the ratio says how much of the contract
/// held rather than which call happened to fail first.
pub async fn context(scenario: &Scenario, embedder: Arc<dyn Embedder>) -> Ratio {
    let mut passed = 0;
    let mut total = 0;
    for case in &scenario.context {
        let seeded = scenario::seed(scenario, embedder.clone()).await;
        let mut arguments = Map::new();
        if let Some(query) = &case.query {
            arguments.insert("query".into(), json!(query));
        }
        if let Some(budget) = case.budget_chars {
            arguments.insert("budget_chars".into(), json!(budget));
        }
        let block = seeded.agmem.context(Value::Object(arguments)).await;

        let mut unit = |ok: bool| {
            total += 1;
            passed += u32::from(ok);
        };

        // Sections appear in the fixed order, each at most once.
        let observed = headings(&block);
        let in_order = observed
            .iter()
            .all(|heading| SECTION_ORDER.contains(heading))
            && SECTION_ORDER
                .iter()
                .filter_map(|heading| observed.iter().position(|seen| seen == heading))
                .is_sorted()
            && observed.len() == observed.iter().collect::<HashSet<_>>().len();
        unit(in_order);

        // The block fits its budget, stated or default.
        let budget = case
            .budget_chars
            .map_or(DEFAULT_BUDGET_CHARS, |chars| chars as usize);
        unit(block.chars().count() <= budget);

        // No claim appears twice.
        unit(
            scenario
                .seeds
                .iter()
                .all(|seed| block.matches(seed.content.as_str()).count() <= 1),
        );

        // Superseded claims stay out.
        unit(scenario.superseded_labels().iter().all(|label| {
            let closed = &scenario
                .seeds
                .iter()
                .find(|seed| seed.label == *label)
                .expect("superseded label names a seed")
                .content;
            !block.contains(closed.as_str())
        }));

        // Verbatim episode text stays out.
        unit(
            scenario
                .seeds
                .iter()
                .filter_map(|seed| seed.episode.as_deref())
                .all(|episode| !block.contains(episode)),
        );

        // Every id the block cites resolves through `inspect`.
        let mut cited_resolve = true;
        for id in cited_ids(&block) {
            let result = seeded.agmem.call("inspect", json!({ "ref": id })).await;
            cited_resolve =
                cited_resolve && result.is_ok_and(|answer| answer.is_error != Some(true));
        }
        unit(cited_resolve);

        for label in &case.must_contain {
            let content = &scenario
                .seeds
                .iter()
                .find(|seed| seed.label == *label)
                .expect("must_contain names a seed")
                .content;
            unit(block.contains(content.as_str()));
        }
        for text in &case.must_not_contain_text {
            unit(!block.contains(text.as_str()));
        }
        seeded.shutdown().await;
    }
    Ratio { passed, total }
}

/// Every backticked token in the block that is shaped like a memory id.
fn cited_ids(block: &str) -> Vec<&str> {
    block
        .split('`')
        .skip(1)
        .step_by(2)
        .filter(|token| token.len() >= 16 && token.chars().all(char::is_alphanumeric))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hand-computed: relevant at ranks 1 and 3 of a five-hit page with three
    /// relevant ids in total. DCG = 1/log2(2) + 1/log2(4) = 1.5; ideal over
    /// three = 1 + 1/log2(3) + 1/log2(4) = 2.1309…; ratio 0.7039.
    #[test]
    fn ndcg_scores_a_page_by_rank_and_ideal() {
        let relevant: HashSet<&str> = ["a", "b", "c"].into_iter().collect();
        let page = ["a", "x", "b", "y", "z"];
        assert_eq!(round4(ndcg(&page, &relevant)), 0.7039);
        // Everything relevant on top is perfect; a relevant hit past the
        // cut-off earns nothing; an empty page earns nothing.
        assert_eq!(ndcg(&["a", "b", "c"], &relevant), 1.0);
        assert_eq!(ndcg(&["x", "y", "z", "w", "v", "a"], &relevant), 0.0);
        assert_eq!(ndcg(&[], &relevant), 0.0);
        // One relevant id at rank 2: 1/log2(3) over an ideal of 1.
        let one: HashSet<&str> = ["a"].into_iter().collect();
        assert_eq!(round4(ndcg(&["x", "a"], &one)), 0.6309);
    }
}
