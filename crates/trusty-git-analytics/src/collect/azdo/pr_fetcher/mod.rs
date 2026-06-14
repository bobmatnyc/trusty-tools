//! Azure DevOps pull-request fetcher (Issue #84).
//!
//! Strategy: scan commit messages for the standard ADO merge-commit format
//! (`Merged PR NNNN:`), extract unique PR IDs, then fetch each PR's metadata
//! and reviewer list via the project-scoped ADO REST endpoint
//! `GET {org}/{project}/_apis/git/pullrequests/{id}`. This endpoint does not
//! require the repository GUID, which keeps configuration minimal.
//!
//! # `merge_commit_sha` emission matrix (issue #96)
//!
//! | `status`     | `mergeStrategy`            | emitted `merge_commit_sha`        | rationale |
//! |--------------|----------------------------|-----------------------------------|----|
//! | `completed`  | `noFastForward` or absent  | `Some(lastMergeCommit.commitId)`  | true merge |
//! | `completed`  | `squash`                   | `None`                            | squash rewrites history |
//! | `completed`  | `rebase`                   | `None`                            | rebase replays commits |
//! | `completed`  | `rebaseMerge`              | `None`                            | rebase-merge not reliable |
//! | any non-`completed` | *                    | `None`                            | preview merge never landed |
//!
//! Why a separate file: the existing `azdo/client.rs` is already large and
//! covers work-item / WIQL flows. PR fetching is an independent surface area
//! (different DB tables, different commit-message regex) and is easier to test
//! in isolation.

pub(crate) mod db;
pub(crate) mod fetcher;
pub(crate) mod types;

// Re-export the public surface so callers use `azdo::pr_fetcher::*`.
pub use db::{extract_pr_ids, get_existing_pr_numbers, upsert_pr, upsert_pr_reviewer};
pub use fetcher::AdoPrFetcher;
pub use types::{AdoPrReviewer, AdoPullRequest};

// Bring imports into scope for the test module.
#[cfg(test)]
use crate::collect::azdo::errors::AzdoError;
#[cfg(test)]
use crate::core::config::AzureDevOpsConfig;
#[cfg(test)]
use types::PrRaw;

#[cfg(test)]
mod tests;
