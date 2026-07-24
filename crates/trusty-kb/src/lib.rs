//! trusty-kb — deterministic personal-knowledge-base markdown-tree maintainer.
//!
//! Why: assistants accumulate personal knowledge (people, projects, places, …)
//! as scattered markdown. A KB tree is only useful if its structure is
//! *deterministic* — same input always yields byte-identical files, READMEs are
//! generated (not hand-drifted) artifacts, and an arbitrary doc tree can be
//! normalised into the model without ever destroying content. This crate is the
//! deterministic file-operation engine for that store, exposed over MCP so an
//! orchestrator (tagent's base assistant, a dream-extraction pass) can maintain
//! it without embedding file logic. There are NO LLM calls in this binary — it
//! is pure, testable file mechanics.
//!
//! What: a library (the schema, store, and converter engine) plus a `serve
//! --stdio` MCP server binary. The library is split so each file stays under
//! the 500-SLOC cap:
//!   - [`schema`]      — pluggable collection/field/relationship profile.
//!   - [`frontmatter`] — hand-rolled `---` split + deterministic YAML render.
//!   - [`entity`]      — Entity model, slugify, deep-merge, wiki-link scan.
//!   - [`roots`]       — per-call root resolution + path confinement.
//!   - [`store`]       — [`store::KbStore`]: status / list / get.
//!   - [`put`]         — create-or-merge entity writes.
//!   - [`reconcile`]   — inverse-edge materialisation (relationship mirror).
//!   - [`structure`]   — idempotent dir + generated-README maintenance.
//!   - [`validate`]    — full-tree lint.
//!   - [`convert`]     — arbitrary-tree → collection-model converter.
//!   - [`mcp`]         — the JSON-RPC dispatch seam + [`mcp::ServerConfig`].
//!   - [`tooldefs`]    — the static `tools/list` descriptors.
//!
//! Test: every tool is exercised through the [`mcp::dispatch_with`] seam in
//! `tests/dispatch_tests.rs` against temp-dir roots; each module additionally
//! carries focused unit tests for its determinism/merge/lint invariants.

pub mod convert;
pub mod entity;
pub mod frontmatter;
pub mod mcp;
pub mod put;
pub mod reconcile;
pub mod roots;
pub mod schema;
pub mod store;
pub mod structure;
pub mod tooldefs;
pub mod validate;

/// The MCP protocol server name reported in `initialize`'s `serverInfo`.
pub const SERVER_NAME: &str = "trusty-kb";
