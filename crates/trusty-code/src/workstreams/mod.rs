//! Workstream domain model + flat JSON persistence + boot reconciliation
//! (DOC-48 Phase 1A, issue #3293).
//!
//! # Spec References
//!
//! - [`SPEC-WS-01~draft`](docs/specs/DOC-48-tcode-workstreams.md#SPEC-WS-01~draft)
//! - [`SPEC-WS-02~draft`](docs/specs/DOC-48-tcode-workstreams.md#SPEC-WS-02~draft)
//! - [`SPEC-WS-03~draft`](docs/specs/DOC-48-tcode-workstreams.md#SPEC-WS-03~draft)
//! - [`SPEC-WS-05~draft`](docs/specs/DOC-48-tcode-workstreams.md#SPEC-WS-05~draft)
//! - [`SPEC-WS-06~draft`](docs/specs/DOC-48-tcode-workstreams.md#SPEC-WS-06~draft)
//! - [`SPEC-WS-07~draft`](docs/specs/DOC-48-tcode-workstreams.md#SPEC-WS-07~draft) AC-7
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
//! daemon's [`crate::binding::ProjectBinding`] (see [`path`]); (issue #3294)
//! [`activation`] — the activation-lock exclusivity layer above the store
//! (`ActiveConflict`/force-switch semantics `WorkstreamStore::set_active`
//! deliberately left out) — [`protocol`], the full `workstream.*`
//! JSON-RPC surface (`create`/`get`/`list`/`close` from #3295 plus
//! `activate`/`deactivate` from #3294, built on [`activation`]); and (issue
//! #3297) [`sse`] — the `GET /workstreams/{id}/events` fan-out aggregation
//! route, built directly on [`store::WorkstreamStore`] (bypassing the
//! JSON-RPC router, mirroring `crate::serve::http::session_events_sse`'s
//! per-session precedent).
//!
//! **Scope note (Phase 1A):** #3293 built ONLY the domain model and
//! persistence layer (this module's `model`/`path`/`store` submodules);
//! #3294 added the activation-lock RPC semantics (`activation`, plus
//! `activate`/`deactivate` in [`protocol`]); #3295 added the rest of
//! `protocol`'s surface (`create`/`get`/`list`/`close`) and the REST wrappers
//! (`crate::serve::rest::workstreams`); #3297 (this ticket) adds [`sse`] —
//! the `GET /workstreams/{id}/events` aggregation route — plus
//! `activation`'s new role as the sole publisher of
//! `crate::events::Event::WorkstreamActivationChanged`. The CLI (#3296) and
//! the `trusty-agents-common` extraction of [`sse`]'s aggregation layer
//! (Phase 1B, #3299) remain out of scope.
//!
//! Test: `cargo test -p trusty-code workstreams::` exercises every submodule
//! in place; see each submodule's own `Test:` line for specifics.

pub mod activation;
pub mod model;
mod path;
pub mod protocol;
pub mod sse;
pub mod store;

pub use activation::{ActivateOutcome, ActivationError, SharedWorkstreamStore};
pub use model::{Workstream, WorkstreamId, WorkstreamState};
pub use path::{default_data_dir, store_path};
pub use sse::WorkstreamEventEnvelope;
pub use store::{ReconcileOutcome, StoreError, WorkstreamStore};
