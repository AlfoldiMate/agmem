//! agmem SurrealDB repository.
//!
//! Owns the connection (embedded or remote via a connection string), the
//! versioned schema migrations, and every SurrealQL query. Callers speak the
//! `agmem-core` domain types; nothing outside this crate writes SurrealQL.
//! See `docs/design.md` §4.
