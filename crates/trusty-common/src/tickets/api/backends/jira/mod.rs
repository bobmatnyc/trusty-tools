//! JIRA Cloud backend (REST API v3).
//!
//! Why: JIRA is the enterprise default; the v3 API uses ADF for prose.
//! What: Basic-auth (email + API token), JQL for queries, Versions for
//! milestones.
//! Test: shape tests in `backend`; live tests gated by env vars.

mod backend;
mod client;
mod types;

pub use types::JiraBackend;
