//! GitHub backend (REST v3 + GraphQL v4).
//!
//! Why: GitHub Issues is the de-facto open-source tracker; we need full
//! CRUD + labels + milestones + Projects V2.
//! What: PAT-authenticated `reqwest` client. REST for issues/comments/
//! labels/milestones; GraphQL for Projects V2.
//! Test: shape tests in `backend`; live tests gated by env vars.

mod backend;
mod client;
mod types;

pub use types::GitHubBackend;
