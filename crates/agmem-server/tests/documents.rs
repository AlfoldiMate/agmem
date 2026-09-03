//! Documents over the wire (#134): a named, typed episode written through
//! `remember`, read back windowed through `inspect`, found by `recall` with
//! its name attached, and purged only on the caller's explicit terms.
//!
//! Every test runs without vectors: none of these behaviours ranks, and the
//! one `recall` here matches on words alone, so no fixture grows.

mod harness;

use std::sync::Arc;

use agmem_embed::NoopEmbedder;
use harness::*;
use serde_json::{Value, json};

/// A `remember` carrying one document and nothing else.
fn document(title: &str, kind: &str, content: &str) -> Value {
    json!({
        "memories": [],
        "episode": {
            "content": content,
            "title": title,
            "doc_kind": kind,
            "tags": ["phase-9"],
            "mime": "text/markdown"
        }
    })
}

/// Content long enough to chunk several times over: `n` paragraphs, each
/// carrying a word only it has, so a query can pick one slice out.
fn long_document(paragraphs: usize) -> String {
    (0..paragraphs)
        .map(|n| format!("Paragraph {n} of the plan. {} gizmo.", "words ".repeat(160)))
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[tokio::test]
async fn a_document_dedupes_by_hash_and_keeps_its_first_name() {
    let agmem = Harness::start(Arc::new(NoopEmbedder)).await;
    let first = agmem
        .remember(document("plan-x", "plan", "Step one.\nStep two."))
        .await;
    let id = first["episode"].as_str().expect("episode id").to_owned();

    // Case and spacing differ; the hash is over normalized text, so this is
    // the same document under a different name — and the first name stands.
    let again = agmem
        .remember(document(
            "plan-x-renamed",
            "review",
            "step ONE.   step two.",
        ))
        .await;
    assert_eq!(again["episode"].as_str(), Some(id.as_str()), "{again}");

    let found = agmem.inspect(&format!("episode:{id}")).await;
    assert_eq!(found["found"]["episode"]["title"], "plan-x", "{found}");
    assert_eq!(found["found"]["episode"]["doc_kind"], "plan");
    assert_eq!(found["found"]["episode"]["mime"], "text/markdown");
    assert_eq!(found["found"]["episode"]["tags"], json!(["phase-9"]));
    assert_eq!(agmem.stats().await.episodes, 1);
    agmem.shutdown().await;
}

#[tokio::test]
async fn a_new_version_under_a_title_supersedes_by_convention() {
    let agmem = Harness::start(Arc::new(NoopEmbedder)).await;
    let mut versions = Vec::new();
    for body in ["version one", "version two", "version three"] {
        let written = agmem.remember(document("plan-x", "plan", body)).await;
        versions.push(written["episode"].as_str().expect("id").to_owned());
    }

    let found = agmem.inspect("doc:current/plan-x").await;
    assert_eq!(found["ref"], "doc:current/plan-x");
    assert_eq!(
        found["found"]["episode"]["id"].as_str(),
        Some(versions[2].as_str()),
        "the newest version is the one the title resolves to: {found}"
    );
    let chain: Vec<&str> = found["found"]["versions"]
        .as_array()
        .expect("versions")
        .iter()
        .map(|v| v["id"].as_str().expect("id"))
        .collect();
    assert_eq!(
        chain,
        [&versions[2], &versions[1], &versions[0]],
        "every version stays readable behind it, newest first"
    );

    // The older versions are still there under their own ids, unchanged.
    let old = agmem.inspect(&format!("episode:{}", versions[0])).await;
    assert_eq!(old["found"]["episode"]["content"], "version one");

    let listed = agmem.inspect("docs").await;
    assert_eq!(
        listed["found"]["documents"]
            .as_array()
            .expect("array")
            .len(),
        3,
        "a listing shows every version, newest first: {listed}"
    );
    assert_eq!(listed["found"]["documents"][0]["id"], versions[2]);

    let missing = agmem
        .call("inspect", json!({ "ref": "doc:current/plan-z" }))
        .await
        .expect_err("an unused title is a miss");
    assert!(refusal(&missing, "plan-z"), "{missing}");
    agmem.shutdown().await;
}

#[tokio::test]
async fn a_document_is_read_through_a_window_and_anonymous_text_whole() {
    let agmem = Harness::start(Arc::new(NoopEmbedder)).await;
    let content = long_document(4);
    let total = content.chars().count();
    let written = agmem
        .remember(document("plan-long", "plan", &content))
        .await;
    let id = written["episode"].as_str().expect("id").to_owned();

    // The default window is one chunk's worth, and says where to go on.
    let first = agmem.inspect(&format!("episode:{id}")).await;
    let window = &first["found"]["window"];
    assert_eq!(window["offset"], 0, "{first}");
    assert_eq!(window["total"], total);
    let returned = window["returned"].as_u64().expect("returned") as usize;
    let chunks = first["found"]["chunks"].as_array().expect("chunks");
    assert!(
        chunks.len() > 1,
        "the fixture chunks several times: {first}"
    );
    assert_eq!(
        returned,
        chunks[0]["chars"].as_u64().expect("chars") as usize,
        "one chunk's worth by default"
    );
    assert_eq!(
        first["found"]["episode"]["content"].as_str().map(str::len),
        Some(returned),
        "the content is the window, not the whole document"
    );
    assert_eq!(first["found"]["episode"]["chars"], total);
    assert_eq!(window["next_offset"], returned);
    assert!(
        chunks.iter().all(|chunk| chunk.get("text").is_none()),
        "a document's chunks carry sizes, not a second copy of the text: {first}"
    );

    // An explicit window is honoured, in characters, and the last page says
    // it is the last.
    let result = agmem
        .call(
            "inspect",
            json!({ "ref": format!("episode:{id}"), "offset": total - 10, "limit": 100 }),
        )
        .await
        .expect("inspect");
    let last = result.structured_content.expect("structured");
    assert_eq!(last["found"]["window"]["returned"], 10, "{last}");
    assert!(last["found"]["window"].get("next_offset").is_none());
    assert_eq!(
        last["found"]["episode"]["content"].as_str(),
        Some(&content[content.len() - 10..])
    );

    // Anonymous text keeps its old shape: whole, chunks with their text.
    let plain = agmem
        .remember(json!({ "memories": [], "episode": { "content": "just a note" } }))
        .await;
    let plain_id = plain["episode"].as_str().expect("id");
    let found = agmem.inspect(&format!("episode:{plain_id}")).await;
    assert!(found["found"].get("window").is_none(), "{found}");
    assert!(found["found"].get("versions").is_none());
    assert_eq!(found["found"]["episode"]["content"], "just a note");
    assert_eq!(found["found"]["chunks"][0]["text"], "just a note");
    agmem.shutdown().await;
}

#[tokio::test]
async fn purging_a_cited_document_is_refused_unless_it_cascades() {
    let agmem = Harness::start(Arc::new(NoopEmbedder)).await;
    let written = agmem
        .remember(json!({
            "memories": [{ "content": "The plan has three steps" }],
            "episode": { "content": "the plan, in full", "title": "plan-x", "doc_kind": "plan" }
        }))
        .await;
    let episode = written["episode"].as_str().expect("id").to_owned();
    let claim = ids(&written["created"])[0].to_owned();
    // A second citer, through `derived_from` rather than `source`.
    let reflected = agmem
        .reflect(json!({
            "insight": "Plans here start with a schema step",
            "derived_from": [format!("episode:{episode}")]
        }))
        .await;
    let insight = reflected["id"].as_str().expect("id").to_owned();

    let refused = agmem
        .call(
            "forget",
            json!({ "ids": [format!("episode:{episode}")], "purge": true }),
        )
        .await
        .expect_err("a cited document does not purge");
    assert!(refusal(&refused, "2 live claim(s)"), "{refused}");
    assert!(
        refusal(&refused, &claim) && refusal(&refused, &insight),
        "{refused}"
    );
    assert_eq!(agmem.stats().await.episodes, 1, "nothing moved");

    let nonsense = agmem
        .call(
            "forget",
            json!({ "ids": [format!("episode:{episode}")], "cascade": true }),
        )
        .await
        .expect_err("cascade without purge means nothing");
    assert!(refusal(&nonsense, "purge: true"), "{nonsense}");

    // The dry run shows the whole blast radius: the document and its citers.
    let preview = agmem
        .forget(json!({
            "ids": [format!("episode:{episode}")], "purge": true, "cascade": true, "dry_run": true
        }))
        .await;
    let mut shown = match_ids(&preview["matched"]);
    shown.sort_unstable();
    let mut expected = vec![episode.as_str(), claim.as_str(), insight.as_str()];
    expected.sort_unstable();
    assert_eq!(shown, expected, "{preview}");
    let cascaded: Vec<&Value> = preview["matched"]
        .as_array()
        .expect("matched")
        .iter()
        .filter(|m| m["cascaded_from"].as_str() == Some(&format!("episode:{episode}")))
        .collect();
    assert_eq!(
        cascaded.len(),
        2,
        "each pulled-in claim names the document: {preview}"
    );

    let purged = agmem
        .forget(json!({
            "ids": [format!("episode:{episode}")], "purge": true, "cascade": true
        }))
        .await;
    let mut gone = ids(&purged["purged"]);
    gone.sort_unstable();
    assert_eq!(gone, expected, "{purged}");
    assert_eq!(purged["chunks_purged"], 1);
    let stats = agmem.stats().await;
    assert_eq!(
        (stats.episodes, stats.memories),
        (0, 0),
        "nothing is left citing text that is gone"
    );

    // An anonymous episode keeps the old rule: its claims outlive it.
    let plain = agmem
        .remember(json!({
            "memories": [{ "content": "The user likes notes" }],
            "episode": { "content": "a note about notes" }
        }))
        .await;
    let plain_id = plain["episode"].as_str().expect("id");
    agmem
        .forget(json!({ "ids": [format!("episode:{plain_id}")], "purge": true }))
        .await;
    assert_eq!(agmem.contents().await, ["The user likes notes"]);
    agmem.shutdown().await;
}

#[tokio::test]
async fn a_verbatim_hit_says_which_document_it_is_a_slice_of() {
    let agmem = Harness::start(Arc::new(NoopEmbedder)).await;
    let written = agmem
        .remember(document("plan-long", "plan", &long_document(3)))
        .await;
    let id = written["episode"].as_str().expect("id").to_owned();
    agmem
        .remember(json!({ "memories": [], "episode": { "content": "an anonymous gizmo note" } }))
        .await;

    let found = agmem.recall(json!({ "query": "gizmo", "k": 10 })).await;
    let hits = hits(&found);
    let from_doc: Vec<&Value> = hits.iter().filter(|hit| hit.get("doc").is_some()).collect();
    assert_eq!(
        from_doc.len(),
        3,
        "every slice of the document names it: {found}"
    );
    for hit in &from_doc {
        assert_eq!(hit["kind"], "episode");
        assert_eq!(hit["doc"]["id"], id);
        assert_eq!(hit["doc"]["title"], "plan-long");
        assert_eq!(hit["doc"]["doc_kind"], "plan");
        assert!(hit["doc"]["position"].is_u64());
    }
    let anonymous = hits
        .iter()
        .find(|hit| hit["content"] == "an anonymous gizmo note")
        .expect("the anonymous slice ranks too");
    assert!(
        anonymous.get("doc").is_none(),
        "anonymous text has no name to give: {anonymous}"
    );
    agmem.shutdown().await;
}

#[tokio::test]
async fn a_document_and_the_claims_drawn_from_it_are_one_source_under_the_cap() {
    let agmem = Harness::start(Arc::new(NoopEmbedder)).await;
    // Six slices of one document and three claims distilled from it all
    // match the query; every one of them keys on the document. Three
    // agent-sourced claims rank below them (BM25 length normalisation:
    // long claims, one term) and would never make a page of five uncapped.
    let written = agmem
        .remember(json!({
            "memories": [
                { "content": "The gizmo plan has six paragraphs" },
                { "content": "The gizmo plan starts with the schema" },
                { "content": "The gizmo plan ends with the eval" }
            ],
            "episode": {
                "content": long_document(6), "title": "plan-long", "doc_kind": "plan"
            }
        }))
        .await;
    let id = written["episode"].as_str().expect("id").to_owned();
    for n in 0..3 {
        let padding = format!("claim {n} {}", "filler ".repeat(400));
        agmem
            .remember(json!({ "memories": [{ "content": format!("{padding} gizmo") }] }))
            .await;
    }

    let found = agmem.recall(json!({ "query": "gizmo", "k": 5 })).await;
    let key = format!("episode:{id}");
    let from_doc = hits(&found)
        .iter()
        .filter(|hit| hit["source"] == key)
        .count();
    assert!(
        from_doc <= 3,
        "one document holds at most cap(5) = 3 of 5 slots: {found}"
    );
    assert!(
        hits(&found).iter().any(|hit| hit["source"] == "agent"),
        "the freed slots went to claims from elsewhere: {found}"
    );
    assert_eq!(found["capped"]["sources"], json!([key]), "{found}");
    agmem.shutdown().await;
}

#[tokio::test]
async fn the_write_path_owns_the_identity_rules() {
    let agmem = Harness::start(Arc::new(NoopEmbedder)).await;
    let untitled = agmem
        .call(
            "remember",
            json!({ "memories": [], "episode": { "content": "x", "doc_kind": "plan" } }),
        )
        .await
        .expect_err("a kind without a title");
    assert!(refusal(&untitled, "episode.title"), "{untitled}");

    let transcript = agmem
        .call(
            "remember",
            json!({
                "space": "user", "memories": [],
                "episode": { "content": "x", "title": "t", "doc_kind": "transcript" }
            }),
        )
        .await
        .expect_err("a transcript in the user space");
    assert!(refusal(&transcript, "user"), "{transcript}");

    // An unknown kind fails at the schema, before the tool runs: the error
    // travels as a tool result rather than a protocol error, and names the
    // spellings that would have worked.
    let bad_kind = agmem
        .call(
            "remember",
            json!({ "memories": [], "episode": { "content": "x", "title": "t", "doc_kind": "memo" } }),
        )
        .await
        .expect("a schema miss is a tool result");
    assert_eq!(bad_kind.is_error, Some(true), "{bad_kind:?}");
    assert!(
        format!("{bad_kind:?}").contains("`plan`"),
        "the refusal lists the valid kinds: {bad_kind:?}"
    );
    agmem.shutdown().await;
}

#[tokio::test]
async fn consolidate_lists_the_documents_nothing_cites() {
    let agmem = Harness::start(Arc::new(NoopEmbedder)).await;
    let cited = agmem
        .remember(json!({
            "memories": [{ "content": "The plan has three steps" }],
            "episode": { "content": "the cited plan", "title": "plan-a", "doc_kind": "plan" }
        }))
        .await;
    let orphan = agmem
        .remember(document("plan-b", "plan", "the orphan plan"))
        .await;

    let listed = agmem.inspect("docs:current").await;
    let documents = listed["found"]["documents"].as_array().expect("documents");
    assert_eq!(documents[0]["id"], orphan["episode"]);
    assert_eq!(documents[0]["cited"], 0, "{listed}");
    assert_eq!(documents[1]["id"], cited["episode"]);
    assert_eq!(documents[1]["cited"], 1);

    let tidy = agmem.consolidate(json!({})).await;
    let orphans = tidy["orphan_documents"].as_array().expect("orphans");
    assert_eq!(orphans.len(), 1, "{tidy}");
    assert_eq!(
        orphans[0]["episode"],
        format!("episode:{}", orphan["episode"].as_str().expect("id"))
    );
    assert_eq!(orphans[0]["title"], "plan-b");
    agmem.shutdown().await;
}
