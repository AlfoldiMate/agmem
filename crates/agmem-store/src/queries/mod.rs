//! Every SurrealQL statement agmem sends, and nothing else.
//!
//! Everything a caller supplies travels as a bound parameter; the text built
//! here only ever varies in *shape* — how many memories a batch holds, which
//! retrieval arms a search has, which filters narrow it.
//!
//! Two engine behaviours are why the text is built rather than written out:
//!
//! - SurrealDB counts `BEGIN` and `COMMIT` as statements, so which response
//!   index carries a request's result depends on how many statements run
//!   before it. [`Builder`] tracks that while building instead of guessing.
//! - The KNN operator's `K` must be an integer literal — a bound parameter is
//!   a parse error — so the candidate pool is formatted into the text.

pub(crate) mod read;
pub(crate) mod reindex;
pub(crate) mod write;

/// A request's text plus the index of the statement carrying its result.
pub(crate) struct Script {
    pub(crate) text: String,
    pub(crate) result_index: usize,
}

/// Collects statements and remembers where the result lands.
pub(crate) struct Builder {
    statements: Vec<String>,
    transaction: bool,
}

impl Builder {
    /// A plain multi-statement request; each statement stands on its own.
    pub(crate) fn plain() -> Self {
        Self {
            statements: Vec::new(),
            transaction: false,
        }
    }

    /// A request wrapped in `BEGIN`/`COMMIT`: it lands whole or not at all.
    pub(crate) fn transaction() -> Self {
        Self {
            statements: vec!["BEGIN".to_owned()],
            transaction: true,
        }
    }

    /// Add a statement that runs before the result.
    pub(crate) fn push(&mut self, statement: impl Into<String>) {
        self.statements.push(statement.into());
    }

    /// Close the request with the statement that produces its result.
    pub(crate) fn finish(mut self, result: String) -> Script {
        self.push(result);
        let result_index = self.statements.len() - 1;
        if self.transaction {
            self.push("COMMIT");
        }
        Script {
            text: format!("{};", self.statements.join(";\n")),
            result_index,
        }
    }
}
