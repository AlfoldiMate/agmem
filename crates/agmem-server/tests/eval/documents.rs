//! The fixture document corpus (issue #137): eighteen real plans, reviews
//! and reports, redacted, checked in under `fixtures/eval/documents/` with a
//! manifest. Seeded ahead of a scenario's own seeds to measure whether
//! confident long prose in the store makes `recall` return worse pages for
//! the claims beside it; `docs/eval/documents.md` carries the bar.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// One manifest row: what `remember` is told about the document.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Document {
    pub title: String,
    pub doc_kind: String,
    #[serde(default)]
    pub tags: Vec<String>,
    /// The file beside the manifest holding the text.
    pub file: String,
    /// Filled from `file` on load; the manifest does not carry it.
    #[serde(skip)]
    pub content: String,
}

fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/eval/documents")
}

/// Every document the manifest lists, in manifest order, text loaded.
pub fn all() -> Vec<Document> {
    let dir = corpus_dir();
    let raw = std::fs::read_to_string(dir.join("manifest.json")).expect("read the manifest");
    let mut documents: Vec<Document> = serde_json::from_str(&raw).expect("the manifest parses");
    assert!(!documents.is_empty(), "the manifest lists no documents");
    for document in &mut documents {
        document.content = std::fs::read_to_string(dir.join(&document.file))
            .unwrap_or_else(|error| panic!("read fixture document {}: {error}", document.file));
    }
    documents
}
