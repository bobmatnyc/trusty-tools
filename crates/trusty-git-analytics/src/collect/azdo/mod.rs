//! Azure DevOps integration (Phases 2–6).
//!
//! Why: groups all Azure DevOps HTTP client code, PR fetching, and supporting
//! types under one module so callers import from `collect::azdo::*`.
//! What: re-exports all public items from sub-modules (`client`, `errors`,
//! `helpers`, `pr_fetcher`, `types`) while keeping internal wire shapes and
//! tests private.
//! Test: `cargo test -p tga` runs all tests in `azdo/tests.rs` and
//! `azdo/pr_fetcher/tests.rs`.

pub mod client;
pub(super) mod errors;
pub(super) mod helpers;
pub mod pr_fetcher;
pub(super) mod types;
pub(super) mod wire;

#[cfg(test)]
mod tests;

// Re-export the full public API so external call sites remain unchanged.
pub use client::AzureDevOpsClient;
pub use errors::AzdoError;
pub use helpers::{extract_work_item_refs, feed_azdo_users, fetch_referenced_work_items};
pub use pr_fetcher::{
    extract_pr_ids, get_existing_pr_numbers, upsert_pr, upsert_pr_reviewer, AdoPrFetcher,
    AdoPrReviewer, AdoPullRequest,
};
pub use types::{
    AzdoComment, AzdoConnectionInfo, AzdoField, AzdoIteration, AzdoProject, AzdoUser, AzdoWorkItem,
    AzdoWorkItemExtended, AzdoWorkItemType, WiqlResult, WorkItem, WorkItemRef,
};
