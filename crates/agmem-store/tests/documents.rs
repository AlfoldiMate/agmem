//! Documents against the real engine (#134): a named, typed episode reads
//! back with its name, versions under a title come newest first, and the
//! citation reads answer through both columns a memory can cite from.

use agmem_core::{Derivation, DocKind, Kind, SpaceName, Writer};
use agmem_store::db::Db;
use agmem_store::repo::{self, Batch, DocumentFilter, NewChunk, NewEpisode, NewMemory, Written};
use agmem_store::{db, migrate};

async fn store() -> Db {
    let db = db::connect("mem://").await.expect("connect mem://");
    migrate::ensure(&db).await.expect("migrate");
    db
}

fn space() -> SpaceName {
    "test".parse().expect("valid slug")
}

/// A document under `title`, chunked as one slice, tagged as given.
fn document(title: &str, kind: DocKind, content: &str, tags: &[&str]) -> NewEpisode {
    let mut episode = NewEpisode::new(content);
    episode.title = Some(title.to_owned());
    episode.doc_kind = Some(kind);
    episode.tags = tags.iter().map(|tag| (*tag).to_owned()).collect();
    episode.mime = Some("text/markdown".to_owned());
    episode.chunks = vec![NewChunk {
        text: content.to_owned(),
        embedding: None,
    }];
    episode
}

/// Write `episode` with `memories` beside it; returns the episode id.
async fn put(db: &Db, episode: NewEpisode, memories: Vec<NewMemory>) -> agmem_core::EpisodeId {
    repo::insert_batch(
        db,
        Batch {
            writer: Writer::default(),
            space: space(),
            episode: Some(episode),
            memories,
        },
    )
    .await
    .expect("write")
    .episode
    .expect("an episode was in the batch")
    .into_id()
}

#[tokio::test]
async fn a_document_reads_back_with_its_name_kind_tags_and_mime() {
    let db = store().await;
    let id = put(
        &db,
        document(
            "plan-x",
            DocKind::Plan,
            "# Plan X\n\nStep one.",
            &["phase-9"],
        ),
        Vec::new(),
    )
    .await;

    let detail = repo::episode(&db, &space(), &id).await.expect("episode");
    assert_eq!(detail.episode.title.as_deref(), Some("plan-x"));
    assert_eq!(detail.episode.doc_kind, Some(DocKind::Plan));
    assert_eq!(detail.episode.tags, ["phase-9"]);
    assert_eq!(detail.episode.mime.as_deref(), Some("text/markdown"));
    assert!(detail.episode.is_document());

    let anonymous = put(&db, NewEpisode::new("just some text"), Vec::new()).await;
    let plain = repo::episode(&db, &space(), &anonymous)
        .await
        .expect("episode");
    assert_eq!(plain.episode.title, None);
    assert_eq!(plain.episode.doc_kind, None);
    assert!(
        plain.episode.tags.is_empty(),
        "NONE on the row reads as empty"
    );
    assert!(!plain.episode.is_document());
}

#[tokio::test]
async fn the_same_normalized_text_is_one_document_carrying_the_first_name() {
    let db = store().await;
    let first = put(
        &db,
        document("plan-x", DocKind::Plan, "Step one.\nStep two.", &[]),
        Vec::new(),
    )
    .await;
    // Case and whitespace differ; the hash is over normalized text.
    let outcome = repo::insert_batch(
        &db,
        Batch {
            writer: Writer::default(),
            space: space(),
            episode: Some(document(
                "plan-x-renamed",
                DocKind::Review,
                "step ONE.   step two.",
                &["later"],
            )),
            memories: Vec::new(),
        },
    )
    .await
    .expect("second write");
    assert_eq!(
        outcome.episode,
        Some(Written::Duplicate(first.clone())),
        "a re-put reports the existing id and rewrites nothing"
    );
    let detail = repo::episode(&db, &space(), &first).await.expect("episode");
    assert_eq!(detail.episode.title.as_deref(), Some("plan-x"));
    assert_eq!(detail.episode.doc_kind, Some(DocKind::Plan));
    assert!(detail.episode.tags.is_empty());
}

#[tokio::test]
async fn versions_under_a_title_come_newest_first() {
    let db = store().await;
    let v1 = put(
        &db,
        document("plan-x", DocKind::Plan, "v1", &[]),
        Vec::new(),
    )
    .await;
    let v2 = put(
        &db,
        document("plan-x", DocKind::Plan, "v2", &[]),
        Vec::new(),
    )
    .await;
    let v3 = put(
        &db,
        document("plan-x", DocKind::Plan, "v3", &[]),
        Vec::new(),
    )
    .await;
    put(&db, document("plan-y", DocKind::Plan, "y", &[]), Vec::new()).await;

    let versions = repo::documents_by_title(&db, &space(), "plan-x")
        .await
        .expect("versions");
    assert_eq!(
        versions.iter().map(|v| v.id.clone()).collect::<Vec<_>>(),
        [v3, v2, v1],
        "the current version is the newest row, and the older ones stay behind it"
    );
    assert!(
        repo::documents_by_title(&db, &space(), "plan-z")
            .await
            .expect("versions")
            .is_empty(),
        "an unused title is an empty list, not an error"
    );
    let elsewhere: SpaceName = "other".parse().expect("valid slug");
    assert!(
        repo::documents_by_title(&db, &elsewhere, "plan-x")
            .await
            .expect("versions")
            .is_empty(),
        "a title is a name inside a space"
    );
}

#[tokio::test]
async fn the_listing_counts_each_citer_once_and_filters_by_kind_and_tag() {
    let db = store().await;
    // A plan cited by two memories through `source.ref`, one of which also
    // cites it through `derived_from`.
    let plan = put(
        &db,
        document("plan-x", DocKind::Plan, "the plan", &["phase-9", "hot"]),
        vec![
            NewMemory::new(Kind::Fact, "the plan has three steps"),
            NewMemory::new(Kind::Fact, "the plan starts with schema"),
        ],
    )
    .await;
    let mut both = NewMemory::new(Kind::Lesson, "plans need a schema step first");
    both.derived_from = vec![Derivation::Episode(plan.clone())];
    let cited_twice = repo::insert_batch(
        &db,
        Batch {
            writer: Writer::default(),
            space: space(),
            episode: Some(document("plan-x", DocKind::Plan, "the plan", &[])),
            memories: vec![both],
        },
    )
    .await
    .expect("write");
    assert_eq!(cited_twice.episode, Some(Written::Duplicate(plan.clone())));
    // A review nobody cites, and an anonymous episode that is not a document.
    let review = put(
        &db,
        document("review-1", DocKind::Review, "the review", &["hot"]),
        Vec::new(),
    )
    .await;
    put(&db, NewEpisode::new("plain text"), Vec::new()).await;

    let filter = DocumentFilter {
        limit: 10,
        ..DocumentFilter::default()
    };
    let listed = repo::documents(&db, &space(), &filter)
        .await
        .expect("documents");
    assert_eq!(
        listed
            .iter()
            .map(|doc| (doc.episode.id.clone(), doc.cited))
            .collect::<Vec<_>>(),
        [(review.clone(), 0), (plan.clone(), 3)],
        "newest first; the memory citing through both columns counts once"
    );

    let plans = repo::documents(
        &db,
        &space(),
        &DocumentFilter {
            kinds: vec![DocKind::Plan],
            ..filter.clone()
        },
    )
    .await
    .expect("documents");
    assert_eq!(plans.len(), 1);
    assert_eq!(plans[0].episode.id, plan);

    let hot = repo::documents(
        &db,
        &space(),
        &DocumentFilter {
            tags: vec!["hot".to_owned()],
            ..filter.clone()
        },
    )
    .await
    .expect("documents");
    assert_eq!(
        hot.len(),
        2,
        "a tag filter keeps every document carrying it"
    );

    let none = repo::documents(
        &db,
        &space(),
        &DocumentFilter {
            tags: vec!["cold".to_owned()],
            ..filter
        },
    )
    .await
    .expect("documents");
    assert!(none.is_empty());
}

#[tokio::test]
async fn citers_and_orphans_read_through_both_columns() {
    let db = store().await;
    let by_source = put(
        &db,
        document("plan-a", DocKind::Plan, "plan a", &[]),
        vec![NewMemory::new(Kind::Fact, "plan a exists")],
    )
    .await;
    let by_derivation = put(
        &db,
        document("plan-b", DocKind::Plan, "plan b", &[]),
        Vec::new(),
    )
    .await;
    let mut insight = NewMemory::new(Kind::Lesson, "plan b taught something");
    insight.derived_from = vec![Derivation::Episode(by_derivation.clone())];
    repo::insert_batch(
        &db,
        Batch {
            writer: Writer::default(),
            space: space(),
            episode: None,
            memories: vec![insight],
        },
    )
    .await
    .expect("write the insight");
    let orphan = put(
        &db,
        document("plan-c", DocKind::Plan, "plan c", &[]),
        Vec::new(),
    )
    .await;

    let citers = repo::document_citers(&db, &space(), &by_derivation)
        .await
        .expect("citers");
    assert_eq!(citers.len(), 1);
    assert_eq!(citers[0].content, "plan b taught something");
    assert_eq!(
        repo::document_citers(&db, &space(), &by_source)
            .await
            .expect("citers")
            .len(),
        1
    );
    assert!(
        repo::document_citers(&db, &space(), &orphan)
            .await
            .expect("citers")
            .is_empty()
    );

    // The episode detail's `derived` walks `derived_from` too, not only
    // `source.ref`.
    let detail = repo::episode(&db, &space(), &by_derivation)
        .await
        .expect("episode");
    assert_eq!(detail.derived.len(), 1);

    let orphans = repo::orphan_documents(&db, &space())
        .await
        .expect("orphans");
    assert_eq!(
        orphans.iter().map(|doc| doc.id.clone()).collect::<Vec<_>>(),
        [orphan],
        "a document cited through either column is not an orphan"
    );

    // Closing the only citer makes an orphan.
    let citer = citers[0].id.clone();
    repo::forget(
        &db,
        &repo::Forget {
            spaces: vec![space()],
            memories: vec![citer],
            episodes: Vec::new(),
            purge: false,
        },
    )
    .await
    .expect("forget");
    let orphans = repo::orphan_documents(&db, &space())
        .await
        .expect("orphans");
    assert_eq!(orphans.len(), 2);
    assert!(
        repo::document_citers(&db, &space(), &by_derivation)
            .await
            .expect("citers")
            .is_empty(),
        "a closed memory no longer holds the document"
    );
}
