//! fpcheck: a Rust-style feedback layer for Free Pascal.
//!
//! The crate is split so that the *feedback loop* (M0) is usable without a full
//! Pascal front end: the FPC runner produces structured diagnostics, the
//! diagnostic engine normalizes and renders them, and the analyzer adds
//! high-confidence lints on top.
//!
//! See `SPEC.md` for the specification this implements.

pub mod analyzer;
pub mod catalog;
pub mod cli;
pub mod config;
pub mod diagnostic;
pub mod error;
pub mod explain;
pub mod fix;
pub mod fpc;
pub mod lexer;
pub mod mcp;
pub mod render;
pub mod source;

pub use diagnostic::{Applicability, Diagnostic, Edit, Label, Level, Source, Span, Suggestion};
pub use error::{Error, Result};

/// Version of the JSON diagnostic schema emitted by `--message-format=json`.
pub const JSON_SCHEMA_VERSION: u32 = 1;

/// Version string used for cache keys and `--version`.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
