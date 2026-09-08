//! Linear project management integration.
//!
//! Provides a GraphQL client for the Linear API to enrich commits that
//! reference Linear issue identifiers (e.g. `ENG-123`, `FE-456`) with the
//! corresponding issue title, status, team, assignee, and priority.

pub mod client;
pub mod sync;

pub use client::{store_linear_issues, LinearClient, LinearIssue, LinearIssuesPage};
