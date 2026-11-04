//! Push loop that feeds concept files to trusty-search and withdraws them
//! (SPEC-OKF-01 §5). Feature `search-feed`; the core stays network-free (B8).
//!
//! Why: a tree write is the truth, but "saved" is not "searchable" (§1.1).
//! The feed is the one place that moves a concept's index state, so every
//! failure path here must leave the journal unchanged and report failure
//! (§5.4, the rule above F1). It replaces trusty-agents'
//! `stores::index_feed` and `index_feed_rpc` (§2.7.2, 3e A), which stay in
//! trusty-agents over the legacy in-bundle journal until the S11 cut-over.
//! What: [`feed`] runs one pass: `search.index.status` preflight (F1, F5,
//! F8, live identity §4.9.2), the §5.3.1 scope check (F16), the
//! [`Bundle::index_backlog`](crate::bundle::Bundle::index_backlog), then per
//! task remove-then-put, reply check (F4, D1 hash), journal append (F3) and
//! the probe (`probe`). [`IndexFeed`] is the transport seam;
//! [`RpcIndexFeed`] is the socket client. Slice S7 of #10746.
//! Test: `crates/trusty-okf/tests/feed_criteria.rs` (criteria 43-51, 60,
//! 63, 133, 134) against a scripted fake daemon, with and without D1.
//!
//! # Spec References
//! - SPEC-OKF-01 §4.9, §4.9.2, §5.2-§5.4, §5.6, held in the trusty-okf
//!   project directory.

use serde::Serialize;
use serde_json::Value;

use crate::OkfError;

mod pass;
pub(crate) mod probe;
mod rpc;
pub(crate) mod wire;

pub use pass::feed;
pub use rpc::{PUSH_TIMEOUT, RpcIndexFeed};
pub use wire::live_identity;

/// A trusty-search call that failed: a transport failure (`code` `None`) or
/// the daemon's own JSON-RPC error.
///
/// #10746: SPEC-OKF-01 §4.4 names `trusty_common::search_rpc::SearchRpcError`;
/// that type carries a daemon code only, so a dead socket (F1) has no value
/// of it. This type keeps the daemon's code when there is one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchError {
    /// The method called.
    pub method: String,
    /// The daemon's JSON-RPC code; `None` for a transport failure.
    pub code: Option<i64>,
    /// The message.
    pub message: String,
}

impl SearchError {
    /// The daemon answered "no such index" (F8).
    pub fn is_not_found(&self) -> bool {
        self.code == Some(trusty_common::search_rpc::CODE_NOT_FOUND)
    }
}

impl std::fmt::Display for SearchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.code {
            Some(c) => write!(f, "{} failed: {} ({c})", self.method, self.message),
            None => write!(f, "{} failed: {}", self.method, self.message),
        }
    }
}

/// The seam between the pass and trusty-search (§5.1).
///
/// Why: every ordering and fail-closed rule of the pass is tested against a
/// scripted fake daemon with no socket; [`RpcIndexFeed`] is the live one.
/// What: one JSON-RPC call; `params` is the full params object, index-scoped
/// envelope included.
#[async_trait::async_trait]
pub trait IndexFeed: Send + Sync {
    /// Call `method` and return its `result`.
    async fn call(&self, method: &str, params: Value) -> Result<Value, SearchError>;
}

/// How a pass ended (§4.2, §5.4). Only `Searchable` is success.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedStatus {
    /// Every task succeeded, every probe passed, the live identity is known
    /// and matches every record, and every concept reads Searchable or
    /// Withdrawn.
    Searchable,
    /// Every push and remove was confirmed, but a probe failed or could not
    /// run, or the live identity is not known (F12, F13, F15).
    Unconfirmed,
    /// Some tasks failed (F2, F4, F7, F10).
    Partial,
    /// Nothing was fed: daemon down, unknown index, a failed scope read, or
    /// every task failed (F1, F8).
    NotFed,
}

/// One failed path, or the pass-level reason when `path` is empty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FeedError {
    /// Bundle-relative file name; empty for a pass-level failure.
    pub path: String,
    /// Why.
    pub message: String,
}

/// What one pass did; each axis has its own count (§4.9 "Reporting rule").
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FeedReport {
    /// How the pass ended.
    pub status: FeedStatus,
    /// Puts the daemon confirmed for exactly the bytes sent.
    pub acknowledged: usize,
    /// Retrieval probes that passed this pass.
    pub searchable: usize,
    /// Removes the daemon confirmed.
    pub removed: usize,
    /// Paths still owed after the pass: failed tasks and unreadable files.
    pub pending: usize,
    /// One entry per failed task, failed or unrunnable probe, or pass-level
    /// reason.
    pub errors: Vec<FeedError>,
}

impl FeedReport {
    /// A pass that could not start: `pending` is the whole backlog (F1, F8).
    pub(crate) fn not_fed(pending: usize, reason: String) -> Self {
        Self {
            status: FeedStatus::NotFed,
            acknowledged: 0,
            searchable: 0,
            removed: 0,
            pending,
            errors: vec![FeedError {
                path: String::new(),
                message: reason,
            }],
        }
    }

    /// The SPEC-OKF-01 §4.10 exit code: 0 only for `Searchable`; 4 (saved,
    /// not fully searchable) for `Unconfirmed`, `Partial` and `NotFed`
    /// (criterion 51).
    pub fn exit_code(&self) -> i32 {
        match self.status {
            FeedStatus::Searchable => 0,
            FeedStatus::Unconfirmed | FeedStatus::Partial | FeedStatus::NotFed => 4,
        }
    }
}

/// The §4.10 exit code of a whole `feed` call (criterion 51).
///
/// What: a report maps through [`FeedReport::exit_code`]; a refusal before
/// any push (`RootMismatch`, `StateDirInBundle`, `PathEscape`) is 2; any
/// other error (`JournalWrite`, `IndexScope`, I/O) is 1.
/// Test: `c51_exit_code_is_non_zero_unless_searchable`.
pub fn exit_code(result: &Result<FeedReport, OkfError>) -> i32 {
    match result {
        Ok(r) => r.exit_code(),
        Err(
            OkfError::RootMismatch { .. } | OkfError::StateDirInBundle(_) | OkfError::PathEscape(_),
        ) => 2,
        Err(_) => 1,
    }
}
