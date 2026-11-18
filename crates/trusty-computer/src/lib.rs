//! trusty-computer: a broker between local AI agents and a bounded cua-driver daemon.
//!
//! Why: the cua-driver daemon holds full Accessibility rights, so agents must
//! reach the desktop only through a component that narrows the tool surface,
//! hides the app menu bar, audits every call and never re-runs a retried call
//! (`docs/plans/trusty-computer-phase2.md`, issue #11460).
//! What: Phase 1 modules. [`policy`] classifies the 56 cua-driver 0.23.2 tools
//! and checks arguments; [`redact`] strips AXMenuBar subtrees and fingerprints
//! a snapshot; [`audit`] is the fail-closed JSONL log; [`journal`] answers a
//! repeated `request_id`. The driver client, supervisor and broker come in
//! later phases.
//! Test: `tests/spike_policy.rs`, `tests/spike_redact.rs`, `tests/spike_audit.rs`,
//! `tests/spike_journal.rs`.

pub mod audit;
pub mod journal;
pub mod policy;
mod policy_rules;
pub mod private;
pub mod pyvalue;
pub mod redact;
