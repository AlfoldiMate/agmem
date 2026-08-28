//! agmem embedding backends.
//!
//! A narrow `Embedder` trait with local implementations: fastembed/ONNX
//! (feature `onnx`, default), model2vec static embeddings (feature `static`),
//! and a no-op backend for BM25-only mode. No network at runtime after the
//! first model fetch. See `docs/design.md` §4.
