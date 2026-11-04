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
//!   - [`schema`]      — pluggable collection/field/relationship profile
//!     (re-exported from trusty-okf, #10746).
//!   - [`frontmatter`] — hand-rolled `---` split + deterministic YAML render
//!     (re-exported from trusty-okf).
//!   - [`entity`]      — Entity model, slugify, deep-merge, wiki-link scan
//!     (re-exported from trusty-okf).
//!   - [`roots`]       — per-call root resolution + path confinement.
//!   - [`store`]       — [`store::KbStore`]: status / list / get.
//!   - [`put`]         — create-or-merge entity writes.
//!   - [`reconcile`]   — inverse-edge materialisation (relationship mirror);
//!     the pure half is in trusty-okf, the file walk in `store_reconcile`.
//!   - [`structure`]   — idempotent dir + generated-README maintenance.
//!   - [`validate`]    — full-tree lint; findings and lint rules are in
//!     trusty-okf, the file walk in `store_validate`.
//!   - [`convert`]     — arbitrary-tree → collection-model converter.
//!   - [`okg`]         — OKG builder: source registry, per-item ledger, and the
//!     idempotent additive ingest engine that grows a tree from doc stores
//!     (in-crate) and Gmail/Drive (fetched by `trusty-agents`, which owns the
//!     authenticated client).
//!   - [`mcp`]         — the JSON-RPC dispatch seam + [`mcp::ServerConfig`].
//!   - [`tooldefs`]    — the static `tools/list` descriptors.
//!
//! Test: every tool is exercised through the [`mcp::dispatch_with`] seam in
//! `tests/dispatch_tests.rs` against temp-dir roots; each module additionally
//! carries focused unit tests for its determinism/merge/lint invariants.

// docs.rs builds a release's documentation once, from the uploaded tarball,
// so a broken intra-doc link is baked into that version forever and only a new
// release can correct it. Deny keeps this crate at zero rather than letting the
// ratchet in `scripts/check_rustdoc_links.sh` absorb a new one.
#![deny(rustdoc::broken_intra_doc_links)]

pub mod convert;
pub mod mcp;
pub mod okg;
pub mod put;
pub mod roots;
pub mod store;
mod store_reconcile;
mod store_validate;
pub mod structure;
pub mod tooldefs;

// #10746 (S2): these five modules moved to trusty-okf (SPEC-OKF-01 §2.7.2).
// trusty-kb re-exports them under their old paths, so trusty-kb and
// trusty-agents keep one implementation until slice S11 deletes this crate.
// The edge runs trusty-kb -> trusty-okf only; boundary rule B1 bars the
// reverse.
pub use trusty_okf::format::concept as entity;
pub use trusty_okf::format::frontmatter;
pub use trusty_okf::format::validate;
pub use trusty_okf::profile::reconcile;

/// The pluggable schema profile and the OKF container rules, now split
/// between `trusty_okf::profile` and `trusty_okf::format::container`
/// (#10746). Re-exported under trusty-kb's old `schema` path until S11.
pub mod schema {
    pub use trusty_okf::format::container::{
        FieldSpec, FieldType, RESERVED_FILES, base_envelope, is_reserved_file,
    };
    pub use trusty_okf::profile::{CollectionSpec, EDGE_PAIRS, Profile, inverse_edge};
}

/// The MCP protocol server name reported in `initialize`'s `serverInfo`.
pub const SERVER_NAME: &str = "trusty-kb";
