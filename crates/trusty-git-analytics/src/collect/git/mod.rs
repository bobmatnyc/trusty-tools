//! Git extraction module.
//!
//! Wraps `git2` to walk repositories, compute per-commit diff statistics,
//! and persist commit + file rows into the SQLite store.

pub mod diff;
pub mod extractor;
pub mod fetch;
pub mod reachability;

pub use diff::{compute_commit_diff, CommitDiff, FileDiff};
pub use extractor::GitCollector;
pub use fetch::fetch_remote;
pub use reachability::{
    build_branch_map, build_tag_map, glob_matches, scan_and_persist, ReachabilityStats,
};
