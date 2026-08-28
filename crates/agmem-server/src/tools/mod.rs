//! One module per MCP tool, attached to [`crate::service::AgmemService`].
//!
//! Empty until the tool issues land. What lives here now is the one thing
//! every tool has to get right and cannot discover on its own: what its
//! annotations must say.
//!
//! # The annotation contract (design §3.1)
//!
//! | tool | annotations |
//! |---|---|
//! | `remember` | `destructive_hint = false, idempotent_hint = true` |
//! | `recall` | `read_only_hint = true, open_world_hint = false` |
//! | `context` | `read_only_hint = true, open_world_hint = false` |
//! | `forget` | `destructive_hint = true` |
//! | `inspect` | `read_only_hint = true, open_world_hint = false` |
//!
//! rmcp omits an unset hint from the wire entirely, and the MCP spec's default
//! for a *missing* `destructiveHint` is **true** — so a tool that says nothing
//! is treated as destructive by every client. Read-only tools must therefore
//! spell out `destructive_hint = false` as well as `read_only_hint = true`;
//! saying only the latter leaves the harmful default standing. The unit test
//! below pins that behaviour, so an rmcp upgrade that changes it fails here
//! rather than quietly loosening what a client believes about `forget`.
//!
//! Annotations are declared on the `#[tool]` attribute itself, in the
//! `#[tool_router]` block of [`crate::service`]:
//!
//! ```ignore
//! #[tool(
//!     name = "recall",
//!     annotations(read_only_hint = true, destructive_hint = false, open_world_hint = false)
//! )]
//! ```

#[cfg(test)]
mod tests {
    use rmcp::model::ToolAnnotations;

    #[test]
    fn an_unset_hint_is_read_as_the_dangerous_default() {
        let silent = ToolAnnotations::new();
        assert!(
            silent.is_destructive(),
            "a tool that declares nothing is destructive; read-only tools must \
             set destructive_hint = false explicitly"
        );
        assert!(!silent.is_idempotent());

        let read_only = ToolAnnotations::new().read_only(true).destructive(false);
        assert!(!read_only.is_destructive());
        assert_eq!(read_only.read_only_hint, Some(true));
    }

    #[test]
    fn unset_hints_stay_off_the_wire() {
        let json = serde_json::to_value(ToolAnnotations::new().read_only(true)).expect("serialize");
        assert_eq!(
            json,
            serde_json::json!({ "readOnlyHint": true }),
            "rmcp omits what was never set, rather than sending the default"
        );
    }
}
