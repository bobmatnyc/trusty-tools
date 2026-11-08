//! Common import interface: `plan_import`, `import`, the ID rule and the
//! import report (SPEC-OKF-01 §2.13-§2.14, §4.10, Appendix A).
//!
//! Why: an import is the ingest engine run against a registered store, not
//! a second engine (§2.13). It must leave the source tree unchanged, mirror
//! its hierarchy under deterministic IDs, refuse collisions instead of
//! guessing, never overwrite a hand edit, never set a protected key from
//! source data, and report saved, index and verification state apart.
//! What: [`plan_import`] lists a source through an [`ImportSource`], assigns
//! IDs by [`id::concept_id_for_source`], picks an extractor and records
//! every refusal, writing nothing. [`import`] applies a plan under the
//! bundle lock: it registers the store, writes each item through the ingest
//! engine (revision line, concept, then ledger line), tombstones only on a
//! deletion-safe listing, and returns an [`ImportReport`]. [`DocTreeSource`]
//! is the one source adapter in this crate (B6); authenticated adapters live
//! outside it. [`import_status`] reads the per-run start and finish lines.
//! Slice S8 of #10746. #10746 S9: [`adapter`] spawns an authenticated
//! source's adapter process with exactly its caller's environment list and
//! speaks the `acquire` role to it; [`record`] normalises its mail and docs
//! items under the Appendix C IDs, provenance and body rules.
//! Test: `crates/trusty-okf/tests/import_criteria.rs` (criteria 71-85, 136,
//! 137, 144), `import_fault_tests` (crash recovery), the feed criteria
//! 85, 129, 135 and 141 in `import_feed_criteria.rs`, and criteria 86-98
//! and 132 in `adapter_criteria.rs` (S9).
//!
//! # Spec References
//! - SPEC-OKF-01 §2.13, §2.13.1, §2.14, §2.15, §4.9, §4.10, Appendix A,
//!   Appendix C.

pub mod adapter;
mod apply;
mod connector;
pub mod id;
mod plan;
pub mod record;
mod report;
mod runlog;
mod source;
mod status;

#[cfg(test)]
mod import_fault_tests;

pub use apply::import;
pub use plan::{
    Action, ContentPolicy, ImportPlan, ImportSpec, MAX_LIST_PAGES, PlannedItem, RunCancel,
    plan_import, plan_import_with,
};
pub(crate) use report::is_refusal;
pub use report::{ImportFinding, ImportReport, ItemReport, exit_code};
pub use source::{DocTreeSource, ImportSource, ListEntry, Listing};
pub use status::{RunStatus, StoreStatus, import_status};
