//! Workstream domain model + flat JSON persistence + boot reconciliation
//! (DOC-48 Phase 1A, issue #3293).
//!
//! # Spec References
//!
//! - [`SPEC-WS-01~draft`](docs/specs/DOC-48-tcode-workstreams.md#SPEC-WS-01~draft)
//! - [`SPEC-WS-02~draft`](docs/specs/DOC-48-tcode-workstreams.md#SPEC-WS-02~draft)
//! - [`SPEC-WS-03~draft`](docs/specs/DOC-48-tcode-workstreams.md#SPEC-WS-03~draft)
//!
//! Why: DOC-39 §4B introduced **Workstream** as a durable named grouping of
//! sessions that must survive daemon restarts, but tcode's `SessionRegistry`
//! is in-memory only — a restart silently loses the workstream list (DOC-48
//! §1.1). This module is the foundation slice that fixes that: a serializable
//! domain object plus a single flat JSON store per project, written with an
//! atomic temp+rename so a crash mid-write never corrupts the file.
//!
//! **Disambiguation (DOC-48 §11):** [`trusty_agents_common::workstreams::Workstream`]
//! is a *different* type — the cross-tool engineering-lead ledger entry
//! (DOC-44). This module's [`Workstream`] is tcode's own native domain object.
//! Code that touches both MUST use qualified imports
//! (`trusty_agents_common::workstreams::Workstream` for the ledger,
//! `trusty_code::workstreams::Workstream` for tcode-native state) — never a
//! bare `use ...::Workstream` in a module that needs both.
//!
//! What: [`WorkstreamId`] (a UUID newtype), [`WorkstreamState`] (the computed
//! `active | idle | closed` enum — never persisted, see §2.2/§3.2),
//! [`Workstream`] (the domain record — see [`model`]); [`WorkstreamStore`]
//! (the flat-JSON, atomic-write, fingerprint-reload store plus boot
//! reconciliation — see [`store`]); path/filename derivation from the
//! daemon's [`crate::binding::ProjectBinding`] (see [`path`]).
//!
//! **Scope note (Phase 1A, issue #3293):** this module implements ONLY the
//! domain model and persistence layer. Activation-lock RPC semantics
//! (`workstream.activate{force}` / `ActiveConflict`, #3294), the RPC/REST
//! surface (#3295), the CLI (#3296), and the SSE aggregation route (#3297)
//! are explicitly out of scope and land in follow-up tickets against this
//! foundation.
//!
//! Test: `cargo test -p trusty-code workstreams::` exercises every submodule
//! in place; see each submodule's own `Test:` line for specifics.

pub mod model;
mod path;
pub mod store;

pub use model::{Workstream, WorkstreamId, WorkstreamState};
pub use path::{default_data_dir, store_path};
pub use store::{ReconcileOutcome, StoreError, WorkstreamStore};
