//! Wire types for the focused delegation guard (SHADOW only, #10892).
//!
//! Why: the tm tap, the outbox, the evaluator and the findings store must
//! agree on one record shape, and the privacy and isolation rules of the
//! design are cheapest to keep when the types enforce them.
//!
//! What: the `types` feature (default, and the only feature in this release):
//!
//! - [`DgEvent`] — `dg.event.v1`, built with [`DgEvent::capture`].
//! - [`PolicyBundle`] — one project's rules, built with [`BundleBuilder`].
//! - [`DgVerdict`] — `dg.verdict.v1`.
//! - [`hash`] — canonical JSON hashing, hashed ids, the dedupe key.
//! - [`redact`] — credential redaction and the 8 KiB excerpt cap.
//!
//! Every parser rejects unknown keys and re-derives every hash it can.
//!
//! Test: `event_round_trips_through_json`, `dedupe_key_changes_with_each_input`,
//! `bundle_for_a_from_b_input_is_refused`, `fixture_labels_match_a1_counts`.
//!
//! # Decision record (ADR style)
//!
//! **Context.** Design comment 6746221317 on #10892 and its amendment A1
//! specify a SHADOW-only guard that observes Agent/Task dispatches and asks
//! an evaluator whether each dispatch fits the project's owner rules. PR1 of
//! the §8 split is this types-only crate.
//!
//! **Decisions.**
//!
//! 1. Types only. No inference, HTTP or model dependency (§4.1), so
//!    `trusty-mpm` can depend on this crate cheaply. The evaluator client and
//!    service arrive later behind a separate `service` feature.
//! 2. `event_id` is a UUID v7 from the workspace `uuid` crate, not the ULID
//!    the design names. Both are time-ordered 128-bit ids; v7 avoids a new
//!    dependency.
//! 3. Redaction is self-contained in [`redact`]. The design points at the
//!    `pm_guard_secret_*` detectors in trusty-mpm; depending on trusty-mpm here
//!    would make a cycle once trusty-mpm depends on this crate.
//! 4. Per-project isolation is checked where records are assembled:
//!    [`BundleBuilder::add_rule`] and [`DgEvent::capture`] refuse input keyed
//!    to another project, and [`DgVerdict::validate_against`] refuses a
//!    citation absent from the event's own bundle.
//! 5. The evaluator never maps an error to allow: [`Status::Unavailable`]
//!    carries a reason and no verdict.
//!
//! **Rejected pattern.** `trusty-mpm`'s `daemon/llm_overseer.rs` also calls a
//! model, but through OpenRouter, and it fails open to Allow on error. That is
//! the wrong account and the wrong failure semantics for this guard; do not
//! reuse it here.
//!
//! **Not settled here.** This crate settles none of the items in the design's
//! UNVERIFIED register (§10).
#![warn(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

#[cfg(feature = "types")]
mod capture;
#[cfg(feature = "types")]
mod error;
#[cfg(feature = "types")]
mod event;
#[cfg(feature = "types")]
pub mod hash;
#[cfg(feature = "types")]
mod policy;
#[cfg(feature = "types")]
mod project;
#[cfg(feature = "types")]
pub mod redact;
#[cfg(feature = "types")]
mod verdict;

#[cfg(feature = "types")]
pub use capture::{CaptureInput, RawIds};
#[cfg(feature = "types")]
pub use error::{DgError, Result};
#[cfg(feature = "types")]
pub use event::{
    Actor, Boundary, DgEvent, DispatchPath, EVENT_SCHEMA, EvidenceHashes, Inherit, InheritVia,
    Proposed, WorkerState,
};
#[cfg(feature = "types")]
pub use hash::{DedupeSet, Seen, Sha256Hex};
#[cfg(feature = "types")]
pub use policy::{
    Authority, BundleBuilder, MAX_RULES, PolicyBundle, PolicyRule, PolicySnapshot,
    RULE_TEXT_MAX_BYTES, RuleScope, RuleSource, RuleStatus, SourceKind, Span,
};
#[cfg(feature = "types")]
pub use project::ProjectId;
#[cfg(feature = "types")]
pub use verdict::{
    DgVerdict, EvidenceNeed, FailureCategory, FundedBy, MISMATCH_MAX_CHARS, ModelReceipt, RefRole,
    RefSpan, RuleRef, Status, Tokens, Unavailable, UnavailableReason, VERDICT_SCHEMA, Verdict,
};
