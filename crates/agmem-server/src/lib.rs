//! agmem server internals, exposed as a library so integration tests (and
//! the in-process MCP protocol tests) can drive them directly.
//!
//! stdout is the MCP wire: nothing in this crate may write to stdout except
//! the protocol transport (enforced by `clippy::print_stdout = deny`).

pub mod config;
pub mod daemon;
pub mod doctor;
pub mod embedder;
pub mod lock;
pub mod prompts;
pub mod service;
pub mod startup;
pub mod telemetry;
pub mod tools;
