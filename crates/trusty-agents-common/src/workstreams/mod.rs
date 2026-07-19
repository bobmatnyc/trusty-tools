//! Heterogeneous workstream ledger — the DOC-44 "engineering lead / virtual
//! twin" architecture's Layer 2 persisted state (issue #3008, twin Phase 2).
//!
//! Why: DOC-44 (`docs/specs/DOC-44-engineering-lead-twin-orchestration.md`)
//! §8 Phase 2 ("Workstream Ledger & Heterogeneous Registry") needs the lead
//! agent (not built by this ticket — DOC-44 §8 Phase 5) to track workstreams
//! across both `tm` and `tcode` in one place. **The issue title says
//! "DOC-42 Phase 2"; `DOC-42` is already claimed by
//! `agent-bundled-skills.md` (shipped in tm 0.19.22) — the correct id is
//! `DOC-44`**, per that spec's own catalog note in `docs/specs/README.md`
//! (the same naming correction Phase 1, issue #3007, already made — see
//! `super::connectors`' module docs). Builds directly on Phase 1
//! (`super::connectors`, issue #3007/PR #3194): [`Harness`] aligns 1:1 with
//! [`super::connectors::BackendParams`]'s `Tm`/`Tcode` variants rather than
//! inventing a third tool-tagging vocabulary.
//!
//! **Relationship to `super::session_registry`:** that module is a
//! *different*, narrower, already-wired registry — `trusty-agents`'
//! `runtime::startup` calls `SessionsRegistry::record_start` on every
//! process boot to log `{run_id, workflow, status}` for cleanup/export
//! tooling (see `crates/trusty-agents/src/runtime/startup.rs`). It is
//! genuinely live (not dead code, despite its own module doc's
//! `#[allow(dead_code)]` scaffolding note, which predates that call site),
//! so it was adapted-around rather than broken: generalizing it in place
//! would require editing `crates/trusty-agents/src/**`, out of this
//! ticket's file scope (multiple agents are active there — see this crate's
//! CLAUDE.md worktree-discipline notes), and it solves a different problem
//! (per-process run tracking, no harness tag, no status lifecycle beyond
//! running/completed/failed) than the DOC-44 workstream ledger (a ticket-level,
//! harness-tagged record with an explicit `WorkstreamStatus` lifecycle).
//! `session_registry` is left untouched; new workstream tracking belongs
//! here.
//!
//! What: [`Harness`], [`WorkstreamStatus`], [`Priority`], [`Workstream`],
//! [`NewWorkstream`] (value types — see [`types`]); [`WorkstreamLedger`] (the
//! JSON-backed create/list/get/query/update registry — see [`ledger`]);
//! [`LedgerError`] (the typed failure surface — see [`error`]);
//! [`LedgerRecovery`]/[`JsonFileRecovery`]/[`TrustyMemoryRecovery`] (the
//! restart-recovery seam — see [`recovery`]).
//! Test: `cargo test -p trusty-agents-common workstreams::` exercises every
//! submodule in place.

mod error;
mod ledger;
mod recovery;
mod types;

pub use error::LedgerError;
pub use ledger::WorkstreamLedger;
pub use recovery::{JsonFileRecovery, LedgerRecovery, TrustyMemoryRecovery};
pub use types::{Harness, NewWorkstream, Priority, Workstream, WorkstreamStatus};
