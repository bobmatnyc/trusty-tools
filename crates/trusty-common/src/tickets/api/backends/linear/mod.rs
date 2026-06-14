//! Linear backend (GraphQL).
//!
//! Why: Linear's API is GraphQL-only; no REST surface.
//! What: All queries are `const &str`. Single `graphql` helper handles
//! auth, payload assembly, and error extraction.
//! Test: shape tests in `backend`; live tests gated by env vars.

mod backend;
mod client;
mod types;

pub use types::LinearBackend;

#[cfg(test)]
mod tests;
