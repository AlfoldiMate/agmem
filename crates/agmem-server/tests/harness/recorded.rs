//! An embedder that replays committed real-BGE vectors (issue #32, #116).
//!
//! The tests want real model semantics — whether "which tool tidies up our
//! source code layout?" lands nearer the ruff claim than the pizza order is
//! exactly what the eval measures, and the write gate, the abstention floor
//! and the consolidate arms all key on cosine — but a live model download in
//! CI would make every number network-dependent and, across ONNX releases,
//! drift-prone. So the vectors are recorded once from the real model and
//! committed: real semantics, bit-stable, offline.
//!
//! Two recordings feed one embedder:
//!
//! - `fixtures/eval/vectors.json` — every text the eval scenarios use,
//!   written by `regenerate_eval_vectors` in `agmem-embed/tests/fastembed.rs`
//!   from the scenario files. Scenario-driven, so a scenario edit regenerates
//!   it wholesale. Its sibling `fixtures/eval/documents-vectors.json` holds
//!   every chunk of the fixture document corpus (issue #137), written by the
//!   same regenerator and merged in here; a separate file so the corpus's
//!   vectors do not churn with every scenario edit.
//! - `fixtures/protocol/vectors.json` — every text the protocol tests write
//!   or ask, which are Rust literals nothing static can enumerate. It grows
//!   by capture: run the suite with `AGMEM_RECORD_VECTORS=1`, and any text
//!   neither recording knows is embedded by the real model and appended.
//!
//! Without that variable, a text nobody recorded is a fixture bug and panics
//! — a silent zero vector would score as a retrieval regression and send
//! whoever reads the diff hunting through the ranking instead of the fixture.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use agmem_embed::{EmbedError, Embedder};
use serde::Deserialize;

/// The command that refreshes the eval recording after a scenario edit.
const REGENERATE_EVAL: &str =
    "cargo test -p agmem-embed --test fastembed -- --ignored regenerate_eval_vectors";

/// Set this to grow the protocol recording from the real model.
const RECORD_ENV: &str = "AGMEM_RECORD_VECTORS";

/// A directory holding `vectors.json` and `documents-vectors.json` recorded
/// with another model (the #133 candidates, `docs/eval/embed-models.md`).
/// When set, the committed recordings are not read at all and the protocol
/// recording is neither consulted nor grown: its texts belong to BGE, and a
/// candidate has no vector for any of them.
const VECTORS_DIR_ENV: &str = "AGMEM_EVAL_VECTORS_DIR";

/// The protocol recording, relative to the crate root; read and, under
/// [`RECORD_ENV`], rewritten at runtime.
const PROTOCOL_FIXTURE: &str = "tests/fixtures/protocol/vectors.json";

/// Passages and queries are separate maps because BGE embeds them
/// differently (queries carry the model's query prefix), so the same text can
/// legitimately have two vectors.
#[derive(Deserialize)]
struct Recording {
    model: String,
    dim: usize,
    passages: BTreeMap<String, Vec<f32>>,
    queries: BTreeMap<String, Vec<f32>>,
}

#[derive(Clone, Copy)]
enum Kind {
    Passage,
    Query,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::Passage => "passage",
            Self::Query => "query",
        }
    }

    fn map(self, recording: &Recording) -> &BTreeMap<String, Vec<f32>> {
        match self {
            Self::Passage => &recording.passages,
            Self::Query => &recording.queries,
        }
    }

    fn map_mut(self, recording: &mut Recording) -> &mut BTreeMap<String, Vec<f32>> {
        match self {
            Self::Passage => &mut recording.passages,
            Self::Query => &mut recording.queries,
        }
    }
}

fn eval() -> &'static Recording {
    static EVAL: OnceLock<Recording> = OnceLock::new();
    EVAL.get_or_init(|| {
        let (mut eval, documents): (Recording, Recording) = match std::env::var_os(VECTORS_DIR_ENV)
        {
            Some(dir) => {
                let dir = PathBuf::from(dir);
                let read = |name: &str| -> Recording {
                    let path = dir.join(name);
                    let raw = std::fs::read_to_string(&path)
                        .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
                    serde_json::from_str(&raw)
                        .unwrap_or_else(|err| panic!("{} parses: {err}", path.display()))
                };
                (read("vectors.json"), read("documents-vectors.json"))
            }
            None => (
                serde_json::from_str(include_str!("../fixtures/eval/vectors.json"))
                    .expect("fixtures/eval/vectors.json parses"),
                serde_json::from_str(include_str!("../fixtures/eval/documents-vectors.json"))
                    .expect("fixtures/eval/documents-vectors.json parses"),
            ),
        };
        assert_eq!(
            (&documents.model, documents.dim),
            (&eval.model, eval.dim),
            "the two eval recordings must come from the same model"
        );
        eval.passages.extend(documents.passages);
        eval.queries.extend(documents.queries);
        eval
    })
}

fn protocol_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(PROTOCOL_FIXTURE)
}

/// The protocol recording, behind a lock because capture appends to it from
/// whichever test misses first.
fn protocol() -> &'static Mutex<Recording> {
    static PROTOCOL: OnceLock<Mutex<Recording>> = OnceLock::new();
    PROTOCOL.get_or_init(|| {
        let recording = match std::fs::read_to_string(protocol_path()) {
            // Another model's recordings: the protocol file is BGE's, so it
            // stays unread, and `replay` panics on any text the override lacks.
            _ if std::env::var_os(VECTORS_DIR_ENV).is_some() => Recording {
                model: eval().model.clone(),
                dim: eval().dim,
                passages: BTreeMap::new(),
                queries: BTreeMap::new(),
            },
            Ok(raw) => serde_json::from_str(&raw).expect("fixtures/protocol/vectors.json parses"),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Recording {
                model: eval().model.clone(),
                dim: eval().dim,
                passages: BTreeMap::new(),
                queries: BTreeMap::new(),
            },
            Err(err) => panic!("read {PROTOCOL_FIXTURE}: {err}"),
        };
        assert_eq!(
            (&recording.model, recording.dim),
            (&eval().model, eval().dim),
            "the two recordings must come from the same model"
        );
        Mutex::new(recording)
    })
}

/// The live model, loaded once and only when capturing.
fn live() -> Option<&'static agmem_embed::fastembed::FastembedBackend> {
    static LIVE: OnceLock<Option<agmem_embed::fastembed::FastembedBackend>> = OnceLock::new();
    LIVE.get_or_init(|| {
        std::env::var_os(RECORD_ENV)?;
        let cache = std::env::temp_dir().join("agmem-model-cache");
        let backend = agmem_embed::fastembed::FastembedBackend::new(
            Some(cache),
            agmem_embed::Accelerator::Cpu,
        )
        .expect("load the real model");
        // The eval recording spells the id as the backend did when it was
        // made; the vectors, not the letter case, are the contract.
        assert_eq!(
            (backend.model_id().to_ascii_lowercase(), backend.dim()),
            (eval().model.to_ascii_lowercase(), eval().dim),
            "the live model must be the one the recordings were made with"
        );
        Some(backend)
    })
    .as_ref()
}

/// Rewrite the protocol recording: one text per line, vectors compact, keys
/// sorted — a capture pass diffs as the texts it added.
fn persist(recording: &Recording) {
    let path = protocol_path();
    std::fs::create_dir_all(path.parent().expect("fixture dir")).expect("create fixture dir");
    let mut out = Vec::new();
    writeln!(out, "{{").unwrap();
    writeln!(
        out,
        "  \"model\": {},",
        serde_json::to_string(&recording.model).unwrap()
    )
    .unwrap();
    writeln!(out, "  \"dim\": {},", recording.dim).unwrap();
    for (section, map, trailing) in [
        ("passages", &recording.passages, ","),
        ("queries", &recording.queries, ""),
    ] {
        writeln!(out, "  \"{section}\": {{").unwrap();
        let last = map.len().saturating_sub(1);
        for (index, (text, vector)) in map.iter().enumerate() {
            let comma = if index == last { "" } else { "," };
            writeln!(
                out,
                "    {}: {}{comma}",
                serde_json::to_string(text).unwrap(),
                serde_json::to_string(vector).unwrap()
            )
            .unwrap();
        }
        writeln!(out, "  }}{trailing}").unwrap();
    }
    writeln!(out, "}}").unwrap();
    std::fs::write(&path, out).unwrap_or_else(|err| panic!("write {}: {err}", path.display()));
}

/// The recorded vector for `text`, capturing it from the live model when
/// [`RECORD_ENV`] is set and no recording has it.
fn replay(kind: Kind, texts: &[String]) -> Vec<Vec<f32>> {
    // A failed test must not poison every test after it: the recording is
    // only ever appended to under the lock, so a panic elsewhere left it whole.
    let mut protocol = protocol()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let missing: Vec<String> = texts
        .iter()
        .filter(|text| {
            !kind.map(eval()).contains_key(*text) && !kind.map(&protocol).contains_key(*text)
        })
        .cloned()
        .collect();
    if !missing.is_empty() {
        let Some(model) = live() else {
            panic!(
                "no recorded {} vector for {:?}; run the suite with {RECORD_ENV}=1 to capture it \
                 (eval scenario texts come from `{REGENERATE_EVAL}` instead)",
                kind.name(),
                missing[0]
            );
        };
        let vectors = match kind {
            Kind::Passage => model.embed_passages(&missing).expect("embed passages"),
            Kind::Query => missing
                .iter()
                .map(|text| model.embed_query(text).expect("embed query"))
                .collect(),
        };
        let map = kind.map_mut(&mut protocol);
        for (text, vector) in missing.into_iter().zip(vectors) {
            map.insert(text, vector);
        }
        persist(&protocol);
    }
    texts
        .iter()
        .map(|text| {
            kind.map(eval())
                .get(text)
                .or_else(|| kind.map(&protocol).get(text))
                .expect("present after capture")
                .clone()
        })
        .collect()
}

/// Replays the recorded vectors; panics on any text the recordings lack
/// unless [`RECORD_ENV`] says to capture it.
#[derive(Debug, Clone, Copy)]
pub struct RecordedEmbedder;

impl Embedder for RecordedEmbedder {
    fn dim(&self) -> usize {
        eval().dim
    }

    fn model_id(&self) -> &str {
        &eval().model
    }

    fn embed_passages(&self, passages: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        Ok(replay(Kind::Passage, passages))
    }

    fn embed_query(&self, query: &str) -> Result<Vec<f32>, EmbedError> {
        Ok(replay(Kind::Query, std::slice::from_ref(&query.to_owned())).remove(0))
    }
}
