//! The #133 candidate probe's two measurements (docs/eval/embed-models.md).
//!
//! Both are ignored and keyed by `AGMEM_CANDIDATE`; run them with
//! `--features candidates --release`, one candidate at a time:
//!
//! - `embed_dump` re-embeds every row of a store dump (`AGMEM_DUMP`, the
//!   JSON array `scripts/pair-rank-probe.py` takes) and writes
//!   `target/eval/<id>-dump-vectors.json` as `{id: vector}`, which the
//!   probe's `--vectors` flag swaps in — so its cosine-control AUC becomes
//!   the candidate's.
//! - `latency` times one 3-sentence claim, a batch of 16 claims and the first
//!   60 chunks of the largest fixture document, warm, and appends p50/p95
//!   rows to `docs/eval/embed-models/latency.json`. The rows carry the
//!   accelerator so #139's CoreML run lands in the same file.

#![cfg(feature = "candidates")]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use agmem_embed::Embedder;
use agmem_embed::candidates::{CANDIDATE_ENV, Candidate, CandidateBackend, cache_dir};

/// The store dump the separation sets are read from.
const DUMP_ENV: &str = "AGMEM_DUMP";

/// Samples per shape after warm-up; `AGMEM_LATENCY_RUNS` overrides it for a
/// model too slow to sample twenty times.
const RUNS: usize = 20;

fn runs() -> usize {
    std::env::var("AGMEM_LATENCY_RUNS")
        .ok()
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(RUNS)
}

/// The 3-sentence claim of bar item 3 — three sentences, an id, a date, a
/// number: the shape of what `remember` stores.
const CLAIM: &str = "The atlas release build targets ubuntu and macos as of PR #212. The \
                     macos runner was added on 2026-03-14 after the linux-only phase. Builds \
                     take about 6 minutes on either.";

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn candidate() -> Candidate {
    Candidate::from_env()
        .unwrap_or_else(|| panic!("{CANDIDATE_ENV} names the candidate to measure"))
}

fn load(candidate: Candidate) -> (CandidateBackend, f64) {
    let started = Instant::now();
    let backend = CandidateBackend::load(candidate, &cache_dir()).expect("load candidate");
    let load_ms = started.elapsed().as_secs_f64() * 1e3;
    eprintln!("{} loaded in {load_ms:.0} ms", candidate.id());
    (backend, load_ms)
}

/// Shortest round-tripping decimal per component.
fn json_vector(vector: &[f32]) -> String {
    let parts: Vec<String> = vector.iter().map(|x| format!("{x}")).collect();
    format!("[{}]", parts.join(","))
}

#[test]
#[ignore = "re-embeds a store dump with the candidate AGMEM_CANDIDATE names"]
fn embed_dump() {
    let candidate = candidate();
    let dump = std::env::var(DUMP_ENV)
        .unwrap_or_else(|_| panic!("{DUMP_ENV} points at the store dump JSON"));
    let rows: Vec<serde_json::Value> =
        serde_json::from_str(&std::fs::read_to_string(&dump).expect("read dump"))
            .expect("the dump is a JSON array");
    let mut ids = Vec::with_capacity(rows.len());
    let mut texts = Vec::with_capacity(rows.len());
    for row in &rows {
        let (Some(id), Some(content)) = (row["id"].as_str(), row["content"].as_str()) else {
            continue;
        };
        ids.push(id.to_owned());
        texts.push(content.to_owned());
    }
    assert!(
        !texts.is_empty(),
        "the dump holds no rows with id and content"
    );
    // The band pairs (docs/eval/bands/pairs.json) are not store rows; they
    // ride along under `band:<name>:a|b` so embed-thresholds.py can place
    // the duplicate gate against them.
    let bands: Vec<serde_json::Value> = serde_json::from_str(
        &std::fs::read_to_string(workspace().join("docs/eval/bands/pairs.json"))
            .expect("read bands"),
    )
    .expect("bands parse");
    for pair in &bands {
        let name = pair["pair"].as_str().expect("pair name");
        for side in ["a", "b"] {
            ids.push(format!("band:{name}:{side}"));
            texts.push(pair[side].as_str().expect("pair text").to_owned());
        }
    }

    let (backend, _) = load(candidate);
    let started = Instant::now();
    let mut vectors = Vec::with_capacity(texts.len());
    // Sliced as the server slices, so a dynamic-quant model sees the batch
    // size it would see in production rather than the whole dump at once.
    for slice in texts.chunks(128) {
        vectors.extend(backend.embed_passages(slice).expect("embed passages"));
    }
    let took = started.elapsed().as_secs_f64();
    let dim = backend.dim();
    assert!(vectors.iter().all(|vector| vector.len() == dim));

    let by_id: BTreeMap<&str, &Vec<f32>> =
        ids.iter().map(String::as_str).zip(vectors.iter()).collect();
    let entries: Vec<String> = by_id
        .iter()
        .map(|(id, vector)| {
            format!(
                "  {}: {}",
                serde_json::to_string(id).unwrap(),
                json_vector(vector)
            )
        })
        .collect();
    let out_dir = workspace().join("target/eval");
    std::fs::create_dir_all(&out_dir).expect("create target/eval");
    let out = out_dir.join(format!("{}-dump-vectors.json", candidate.id()));
    std::fs::write(&out, format!("{{\n{}\n}}\n", entries.join(",\n"))).expect("write vectors");
    eprintln!(
        "{}: {} rows, {dim}d, {took:.1}s → {}",
        candidate.id(),
        by_id.len(),
        out.display()
    );
}

/// Median and 95th percentile of `samples`, in milliseconds.
fn percentiles(mut samples: Vec<f64>) -> (f64, f64) {
    samples.sort_by(f64::total_cmp);
    let at = |q: f64| samples[((samples.len() - 1) as f64 * q).round() as usize];
    (at(0.5), at(0.95))
}

fn time<F: FnMut()>(mut run: F) -> (f64, f64) {
    for _ in 0..3 {
        run();
    }
    let samples: Vec<f64> = (0..runs())
        .map(|_| {
            let started = Instant::now();
            run();
            started.elapsed().as_secs_f64() * 1e3
        })
        .collect();
    percentiles(samples)
}

fn chip() -> String {
    std::process::Command::new("sysctl")
        .args(["-n", "machdep.cpu.brand_string"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
        .unwrap_or_else(|| std::env::consts::ARCH.to_owned())
}

#[test]
#[ignore = "times the candidate AGMEM_CANDIDATE names on this machine"]
fn latency() {
    let candidate = candidate();
    let accelerator = std::env::var("AGMEM_ACCELERATOR").unwrap_or_else(|_| "cpu".to_owned());
    let (backend, load_ms) = load(candidate);

    let claims: Vec<String> = (0..16)
        .map(|i| format!("{CLAIM} Variant {i} of the same claim, so no two batch rows are equal."))
        .collect();
    // Sixty chunks of fixture prose, in manifest order: no single fixture
    // document has that many, so the corpus is walked until it does.
    let documents_dir = workspace().join("crates/agmem-server/tests/fixtures/eval/documents");
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(documents_dir.join("manifest.json")).expect("read manifest"),
    )
    .expect("manifest parses");
    let mut chunks: Vec<String> = Vec::new();
    for entry in manifest.as_array().into_iter().flatten() {
        let file = entry["file"].as_str().expect("manifest file");
        let text = std::fs::read_to_string(documents_dir.join(file)).expect("read document");
        chunks.extend(agmem_core::chunk::chunk(&text));
        if chunks.len() >= 60 {
            break;
        }
    }
    chunks.truncate(60);
    assert_eq!(
        chunks.len(),
        60,
        "the fixture corpus has fewer than 60 chunks"
    );

    let one = vec![CLAIM.to_owned()];
    let shapes: Vec<(&str, Vec<String>)> = vec![
        ("claim", one),
        ("claims-16", claims),
        ("document-60-chunks", chunks),
    ];
    let mut rows = Vec::new();
    for (shape, texts) in &shapes {
        let (p50, p95) = time(|| {
            backend.embed_passages(texts).expect("embed");
        });
        eprintln!(
            "{} {accelerator} {shape}: p50 {p50:.1} ms, p95 {p95:.1} ms",
            candidate.id()
        );
        rows.push(serde_json::json!({
            "model": candidate.id(),
            "accelerator": accelerator,
            "shape": shape,
            "p50_ms": (p50 * 10.0).round() / 10.0,
            "p95_ms": (p95 * 10.0).round() / 10.0,
            "load_ms": load_ms.round(),
            "chip": chip(),
            "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
            "measured_at": jiff_now(),
        }));
    }

    let path = workspace().join("docs/eval/embed-models/latency.json");
    std::fs::create_dir_all(path.parent().unwrap()).expect("create docs/eval/embed-models");
    let mut all: Vec<serde_json::Value> = std::fs::read_to_string(&path)
        .ok()
        .map(|raw| serde_json::from_str(&raw).expect("latency.json parses"))
        .unwrap_or_default();
    // One row per (model, accelerator, shape): a re-run replaces, never stacks.
    all.retain(|row| {
        !rows.iter().any(|new| {
            ["model", "accelerator", "shape"]
                .iter()
                .all(|key| row[key] == new[key])
        })
    });
    all.extend(rows);
    std::fs::write(&path, serde_json::to_string_pretty(&all).unwrap() + "\n").expect("write");
    eprintln!("→ {}", path.display());
}

/// RFC3339 minute precision without pulling a date crate into the probe.
fn jiff_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    // Days since epoch → civil date (Howard Hinnant's algorithm).
    let days = (secs / 86_400) as i64;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let rem = secs % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60
    )
}
