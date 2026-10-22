//! The Architect dashboard store: #10063 PR 1, the store core.
//!
//! Why: the dashboard's documents live in a Python prototype store
//! (`dashboard_store.py` and friends). DOC-82 wants one implementation, in
//! this crate, that the tm daemon hosts (LDG-1.1, owner ruling Dbc), and it
//! must leave the prototype's bytes until the prototype is removed (LDG-1.5).
//! What: [`store`] (the [`Store`] in the `dir` and `jsonl` formats, locking,
//! versions, all-or-nothing batches), [`canonical`] (the two byte
//! encodings), [`ids`] (path-safe names and the recycled id space),
//! [`derive`](mod@derive) and [`tally`](mod@tally) (the one count path and the footer), and
//! [`schema_check`] (the schema hook and the keyword-subset validator).
//! Not here yet: the change log, the prune archive's file, `fsck`,
//! `validate` (PR 2); verbs, daemon methods, MCP and CLI (PR 3).
//! Test: `tests/dashboard_golden.rs`, `tests/dashboard_store.rs`,
//! `tests/dashboard_tally.rs`.
//!
//! # Spec References
//! - [`SPEC-LEDGER-02~draft`](docs/specs/DOC-82-portfolio-intent-ledger-dashboard.md#SPEC-LEDGER-02~draft)
//! - [`SPEC-LEDGER-05~draft`](docs/specs/DOC-82-portfolio-intent-ledger-dashboard.md#SPEC-LEDGER-05~draft)

pub mod canonical;
pub mod derive;
mod error;
pub mod ids;
pub mod schema_check;
pub mod store;
pub mod tally;

pub use error::DashError;
pub use schema_check::{SchemaDir, SchemaMode, SchemaSet};
pub use store::{DeleteArchive, Doc, Format, ListQuery, Op, OpResult, Removed, Snapshot, Store};
pub use tally::{Footer, FooterLinks, Tally, footer, tally, tally_line};
