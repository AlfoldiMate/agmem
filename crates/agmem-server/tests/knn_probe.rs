//! Issue #40: a `KnnScan` under-returns when a predicate is pushed into it.
//!
//! Ignored by default: these load the real embedding model, or the fixture it
//! wrote, because the fault only shows for a real BGE *query* embedding — a
//! stored row's own vector, or a random one, returns the full set. That is the
//! first of two reasons it read as unreproducible four times at #39.
//!
//! The second is that the state is per **process**. A connection that has
//! never run an *unfiltered* KNN serves a filtered one short; one unfiltered
//! scan repairs every filtered scan after it for that connection's life, while
//! repeating the filtered scan does not. So any probe that measures the
//! unfiltered arm first has already destroyed what it came to observe — and
//! agmem, whose vector arm always carries `space IN $spaces AND invalid_at IS
//! NONE`, never warms itself and is short on every recall a process serves.
//!
//! Run with `cargo test -p agmem-server --test knn_probe -- --ignored`.

use agmem_core::{Kind, SpaceName, Writer};
use agmem_embed::Embedder;
use agmem_embed::fastembed::FastembedBackend;
use agmem_store::db::Db;
use agmem_store::repo::{self, Batch, NewMemory, Search};
use agmem_store::{db, migrate};

/// The `K` and `EF` the read path uses (`repo::DEFAULT_POOL`, `EF_SEARCH`).
const K: usize = 64;
const EF: usize = 80;

/// The conjuncts agmem's own vector arm carries, verbatim.
const FILTER: &str = "space IN $spaces AND invalid_at IS NONE AND ";

/// The space under the filter.
const PROBE: &str = "probe";
/// A second space, so `space IN $spaces` actually excludes something.
const OTHER: &str = "other";

/// Real model output for the two rows and the three questions of the repro
/// store, captured by `agmem-embed`'s `regenerate_knn_fixture`.
const FIXTURE: &str = include_str!("fixtures/knn_underreturn.json");

/// Rows in the searched space. Deliberately varied: a graph of near-identical
/// vectors has no interesting traversal to get wrong.
const PROBE_ROWS: [&str; 40] = [
    "The user formats Python with black.",
    "This project has moved off black for Python code formatting and now uses ruff format instead.",
    "Rust is the language of choice for anything performance sensitive here.",
    "The build runs on a two-machine matrix, ubuntu and macos.",
    "Clippy is configured to deny warnings in continuous integration.",
    "The team stands up every Tuesday at half past nine.",
    "Database migrations are numbered and never edited after landing.",
    "The staging environment mirrors production except for the payment provider.",
    "Secrets live in the platform keychain, never in the repository.",
    "Log output goes to stderr because stdout carries the protocol.",
    "The kitchen tap has been dripping since Tuesday.",
    "Somebody should water the plants by the window.",
    "Coffee is made fresh at eleven and nobody touches the second pot.",
    "The bicycle in the hallway belongs to the person in accounts.",
    "Parking is free after six on the street behind the building.",
    "The user prefers dark mode in every editor they open.",
    "Tabs are four spaces wide in this codebase, set by the formatter.",
    "Commit messages describe why, not what, and never exceed the subject line.",
    "Pull requests need one approving review before merge.",
    "The changelog is generated from commit trailers at release time.",
    "Gardening in spring mostly means being patient with the soil.",
    "Tomatoes want more sun than the balcony gets in the morning.",
    "The compost bin needs turning about once a fortnight.",
    "Slugs found the lettuce again this year.",
    "Bulbs go in the ground in October if the frost holds off.",
    "The borrow checker took an hour to explain to the new starter.",
    "Lifetimes are easier to teach after ownership has landed properly.",
    "Async traits stopped needing a macro two editions ago.",
    "A trait object costs a virtual call and buys a smaller binary.",
    "Iterators fuse into one loop when the optimiser can see through them.",
    "The train to the coast leaves from platform four on weekends.",
    "Tickets bought at the machine are cheaper than tickets bought aboard.",
    "The last connection back is twenty past eleven, not midnight.",
    "The line closes for engineering work most of August.",
    "A bicycle counts as luggage outside peak hours.",
    "Bread wants a wetter dough than most recipes admit.",
    "The oven runs about fifteen degrees hot on the top shelf.",
    "Sourdough starter survives a fortnight in the fridge unfed.",
    "Salt goes in after the first rest, never with the yeast.",
    "A cast iron pan should never see soap or a dishwasher.",
];

/// Rows the `space` filter must exclude.
const OTHER_ROWS: [&str; 8] = [
    "The invoice numbering restarts every financial year.",
    "Expense claims over fifty need a receipt photographed.",
    "The office printer jams on anything heavier than 120gsm.",
    "Visitor badges are returned to the desk, not the bin.",
    "Fire drills happen on the first Wednesday of the quarter.",
    "The meeting room projector needs the grey cable, not the black one.",
    "Milk in the fridge is communal unless it has a name on it.",
    "The door code changes whenever somebody leaves.",
];

/// Questions sharing few or no words with the rows they should find — the
/// shape that isolates the vector arm, since the fulltext arm is empty for
/// them (issue #39).
const QUERIES: [&str; 12] = [
    "which tool tidies up source layout automatically",
    "how is our source styled these days",
    "what did we switch our layout helper to",
    "when does everyone get together each week",
    "where are credentials supposed to be kept",
    "what grows badly on a shaded balcony",
    "how long can a fermenting culture go unfed",
    "what is the cheapest way to travel at the weekend",
    "why does a compiler complain about references outliving their owner",
    "what happens to a saucepan in the dishwasher",
    "who reviews changes before they land",
    "what colour scheme does this person like",
];

/// Framings that multiply the corpus without repeating a vector, so the sweep
/// runs where `K` binds as well as where it does not. Each is written in its
/// own session, the way a store fills up in real use.
const FRAMINGS: [&str; 8] = [
    "",
    "Note: ",
    "Remember that ",
    "The team agreed that ",
    "It was mentioned that ",
    "For the record, ",
    "As of last quarter, ",
    "A reminder: ",
];

fn space(name: &str) -> SpaceName {
    name.parse().expect("valid slug")
}

/// `(text, embedding)` pairs under `key` in the fixture.
fn fixture(key: &str) -> Vec<(String, Vec<f32>)> {
    let document: serde_json::Value = serde_json::from_str(FIXTURE).expect("fixture json");
    document[key]
        .as_array()
        .expect("fixture array")
        .iter()
        .map(|entry| {
            let text = entry
                .get("content")
                .or_else(|| entry.get("text"))
                .and_then(serde_json::Value::as_str)
                .expect("fixture text");
            let embedding = entry["embedding"]
                .as_array()
                .expect("fixture embedding")
                .iter()
                .map(|component| component.as_f64().expect("component") as f32)
                .collect();
            (text.to_owned(), embedding)
        })
        .collect()
}

/// Opens `url` on a connection that has run no query of its own.
///
/// surrealkv allows one process at a time per data dir and releases the lock
/// asynchronously when the last handle drops, so a cold reading waits for the
/// previous one to let go rather than opening alongside it.
async fn connect_cold(url: &str) -> Db {
    let mut last = None;
    for _ in 0..50 {
        match db::connect(url).await {
            Ok(db) => return db,
            Err(error) => {
                last = Some(error);
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    }
    panic!("cold connect never got the lock: {last:?}");
}

/// The memory ids one KNN arm returns, sorted.
///
/// `predicate` is the conjunct list preceding the KNN operator, ending in its
/// own `AND ` — empty for the unfiltered arm. Both parameters are always
/// bound, whether or not the text uses them.
async fn arm(db: &Db, predicate: &str, vector: &[f32], spaces: &[&str]) -> Vec<String> {
    let sql = format!(
        "SELECT VALUE record::id(id) FROM memory
         WHERE {predicate}embedding <|{K},{EF}|> $vector"
    );
    ids(db, sql, vector, spaces).await
}

/// The candidate mitigation: the KNN runs unfiltered in a subquery and the
/// conjuncts are applied to its result, so nothing is pushed into the scan.
async fn outside(db: &Db, vector: &[f32], spaces: &[&str]) -> Vec<String> {
    over_fetching(db, K, vector, spaces).await
}

/// The mitigation drawing `inner` candidates before filtering.
///
/// The scan no longer knows what the caller wants, so every row it returns
/// from another space — or a superseded one — is a candidate the pool does not
/// get. Over-fetching buys those back; this is what says by how much.
async fn over_fetching(db: &Db, inner: usize, vector: &[f32], spaces: &[&str]) -> Vec<String> {
    let sql = format!(
        "SELECT VALUE record::id(id) FROM
             (SELECT id, space, invalid_at FROM memory
              WHERE embedding <|{inner},{EF}|> $vector)
         WHERE space IN $spaces AND invalid_at IS NONE
         LIMIT {K}"
    );
    ids(db, sql, vector, spaces).await
}

async fn ids(db: &Db, sql: String, vector: &[f32], spaces: &[&str]) -> Vec<String> {
    let scopes: Vec<String> = spaces.iter().map(|name| (*name).to_owned()).collect();
    let mut response = db
        .query(sql)
        .bind(("vector", vector.to_vec()))
        .bind(("spaces", scopes))
        .await
        .expect("knn arm");
    let mut found = response.take::<Vec<String>>(0).expect("ids");
    found.sort();
    found
}

/// Writes `memories` into `space` on a session of its own, then lets go of the
/// store.
async fn write_session(url: &str, name: &str, memories: Vec<NewMemory>) {
    let db = connect_cold(url).await;
    migrate::ensure(&db).await.expect("migrate");
    repo::insert_batch(
        &db,
        Batch {
            space: space(name),
            episode: None,
            memories,
            writer: Writer::default(),
        },
    )
    .await
    .expect("seed batch");
}

fn embedded(content: impl Into<String>, embedding: Vec<f32>) -> NewMemory {
    let mut memory = NewMemory::new(Kind::Fact, content);
    memory.embedding = Some(embedding);
    memory
}

/// The repro store, rebuilt: two live memories in one space, written one per
/// session, then read by a process that wrote neither.
#[tokio::test]
#[ignore = "reads a fixture of real model output"]
async fn a_cold_filtered_arm_finds_both_rows() {
    let rows = fixture("rows");
    let queries = fixture("queries");
    assert_eq!(rows.len(), 2, "the fixture is the two-row repro store");

    let directory = tempfile::tempdir().expect("tempdir");
    let url = format!(
        "surrealkv://{}",
        directory.path().join("agmem.db").display()
    );
    for (content, embedding) in &rows {
        write_session(
            &url,
            PROBE,
            vec![embedded(content.clone(), embedding.clone())],
        )
        .await;
    }

    let mut faults: Vec<String> = Vec::new();
    for (text, vector) in &queries {
        // Cold, and filtered first: an unfiltered scan anywhere earlier in
        // this connection's life would repair the one under test.
        let cold = connect_cold(&url).await;
        let filtered = arm(&cold, FILTER, vector, &[PROBE]).await;
        let tautology = arm(&cold, "1 = 1 AND ", vector, &[PROBE]).await;
        let unfiltered = arm(&cold, "", vector, &[PROBE]).await;
        let warmed = arm(&cold, FILTER, vector, &[PROBE]).await;
        drop(cold);

        let second = connect_cold(&url).await;
        let mitigated = outside(&second, vector, &[PROBE]).await;
        drop(second);

        for (label, got) in [
            ("filtered", &filtered),
            ("1 = 1", &tautology),
            ("unfiltered", &unfiltered),
            ("filtered, warmed", &warmed),
            ("subquery", &mitigated),
        ] {
            if got.len() != rows.len() {
                faults.push(format!(
                    "{text:?}: `{label}` returned {} of {}",
                    got.len(),
                    rows.len()
                ));
            }
        }
    }

    assert!(
        faults.is_empty(),
        "a cold vector arm lost rows to its own predicate:\n{}",
        faults.join("\n")
    );
}

/// The same readings against a store already on disk, named by
/// `AGMEM_KNN_STORE` — the escape hatch for a store that shows the fault when
/// a rebuilt one does not, since what the engine kept on disk is the one thing
/// a fixture cannot carry. Skipped when the variable is unset.
#[tokio::test]
#[ignore = "needs AGMEM_KNN_STORE pointing at a store to probe"]
async fn a_store_on_disk_reads_the_same_cold() {
    let Ok(path) = std::env::var("AGMEM_KNN_STORE") else {
        return;
    };
    let url = format!("surrealkv://{path}");
    let spaces: Vec<&str> = std::env::var("AGMEM_KNN_SPACES")
        .map(|names| names.split(',').map(str::to_owned).collect::<Vec<String>>())
        .unwrap_or_else(|_| vec![PROBE.to_owned()])
        .leak()
        .iter()
        .map(String::as_str)
        .collect();

    let mut report: Vec<String> = Vec::new();
    for (text, vector) in fixture("queries") {
        let cold = connect_cold(&url).await;
        let filtered = arm(&cold, FILTER, &vector, &spaces).await;
        let tautology = arm(&cold, "1 = 1 AND ", &vector, &spaces).await;
        let unfiltered = arm(&cold, "", &vector, &spaces).await;
        let warmed = arm(&cold, FILTER, &vector, &spaces).await;
        drop(cold);

        let second = connect_cold(&url).await;
        let mitigated = outside(&second, &vector, &spaces).await;
        drop(second);

        // What agmem's own read path makes of the same store, cold: the
        // question the raw arms above only stand in for.
        let third = connect_cold(&url).await;
        let mut request = Search::new(spaces.iter().map(|name| space(name)).collect());
        request.vector = Some(vector.clone());
        request.episodes = false;
        let recalled = repo::search_hybrid(&third, &request).await.expect("recall");
        drop(third);

        // Only `search_hybrid` is held to account: the pushed-down arm is
        // *expected* to come back short until the engine is fixed, and the
        // numbers beside it are what says whether that is still true.
        if recalled.len() != unfiltered.len() {
            report.push(format!(
                "{text:?}: filtered {} / `1 = 1` {} / unfiltered {} / warmed {} / \
                 subquery {} / search_hybrid {}",
                filtered.len(),
                tautology.len(),
                unfiltered.len(),
                warmed.len(),
                mitigated.len(),
                recalled.len()
            ));
        }
    }

    assert!(
        report.is_empty(),
        "{path}: agmem's read path lost rows the engine could still see:\n{}",
        report.join("\n")
    );
}

/// The same question on a store big enough for `K` to bind, to say whether the
/// loss is one row or a proportion of the pool.
#[tokio::test]
#[ignore = "loads the real embedding model"]
async fn a_cold_vector_arm_reads_as_a_warm_one_does() {
    let cache = std::env::temp_dir().join("agmem-model-cache");
    let embedder = FastembedBackend::new(Some(cache)).expect("load model");
    let directory = tempfile::tempdir().expect("tempdir");

    let mut faults: Vec<String> = Vec::new();
    let mut cost: Vec<String> = Vec::new();
    let mut probes = 0_usize;
    for scale in [1, FRAMINGS.len()] {
        let rows = (PROBE_ROWS.len() + OTHER_ROWS.len()) * scale;
        let url = format!(
            "surrealkv://{}",
            directory.path().join(format!("scale-{scale}.db")).display()
        );
        // One session per framing, one batch per space: the repro store was
        // filled by successive agmem sessions, not by a single transaction.
        for framing in FRAMINGS.iter().take(scale) {
            for (name, texts) in [
                (PROBE, PROBE_ROWS.as_slice()),
                (OTHER, OTHER_ROWS.as_slice()),
            ] {
                let passages: Vec<String> = texts
                    .iter()
                    .map(|text| format!("{framing}{text}"))
                    .collect();
                let vectors = embedder.embed_passages(&passages).expect("embed passages");
                let memories = passages
                    .into_iter()
                    .zip(vectors)
                    .map(|(content, embedding)| embedded(content, embedding))
                    .collect();
                write_session(&url, name, memories).await;
            }
        }

        for query in QUERIES {
            probes += 1;
            let vector = embedder.embed_query(query).expect("embed query");
            let at = format!("n={rows} {query:?}");

            let cold = connect_cold(&url).await;
            let filtered = arm(&cold, FILTER, &vector, &[PROBE]).await;
            let bare = arm(&cold, "", &vector, &[PROBE]).await;
            let warmed = arm(&cold, FILTER, &vector, &[PROBE]).await;

            if filtered != warmed {
                faults.push(format!(
                    "{at}: filtered returned {} cold and {} warm, {} unfiltered",
                    filtered.len(),
                    warmed.len(),
                    bare.len()
                ));
            }

            // What the mitigation costs, on a warm connection so the
            // comparison is against a healthy engine rather than the fault.
            let started = std::time::Instant::now();
            let pushed = arm(&cold, FILTER, &vector, &[PROBE]).await;
            let pushed_us = started.elapsed().as_micros();
            let started = std::time::Instant::now();
            let plain = over_fetching(&cold, K, &vector, &[PROBE]).await;
            let plain_us = started.elapsed().as_micros();
            let started = std::time::Instant::now();
            let wide = over_fetching(&cold, K * 4, &vector, &[PROBE]).await;
            let wide_us = started.elapsed().as_micros();
            drop(cold);

            cost.push(format!(
                "| {rows} | {query} | {} | {pushed_us} | {} | {plain_us} | {} | {wide_us} |",
                pushed.len(),
                plain.len(),
                wide.len()
            ));
        }
    }

    let report = format!(
        "| rows | query | pushed | µs | subquery k={K} | µs | subquery k={} | µs |\n\
         |---|---|---|---|---|---|---|---|\n{}\n",
        K * 4,
        cost.join("\n")
    );
    std::fs::write(std::env::temp_dir().join("agmem-knn-cost.md"), report).expect("write report");

    assert!(
        faults.is_empty(),
        "the vector arm reads differently cold ({} faults over {probes} probes):\n{}",
        faults.len(),
        faults.join("\n")
    );
}
