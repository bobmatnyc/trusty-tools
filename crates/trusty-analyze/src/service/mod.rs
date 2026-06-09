//! Sidecar HTTP daemon for trusty-analyzer.
//!
//! Why: Keeps analysis isolated from trusty-search. The daemon fetches chunks
//! from the search daemon over HTTP (`TrustySearchClient::get_chunks`) and
//! computes complexity / smells / quality / facts in-process. It does not
//! talk to trusty-search's redb files directly — the search daemon is the
//! single source of truth for chunk data.
//!
//! What: Thin coordinator module. Declares the submodules and re-exports the
//! public surface so callers import from `service` rather than the internal
//! submodules. The full HTTP surface is:
//! - `GET  /health`
//! - `GET  /sse`                                SSE push stream
//! - `GET  /indexes`                            proxy to trusty-search
//! - `GET  /indexes/{id}/complexity_hotspots`   top-N by cyclomatic
//! - `GET  /indexes/{id}/smells`                chunks with at least one smell
//! - `GET  /indexes/{id}/quality`               aggregate report
//! - `GET  /indexes/{id}/diagnostics`           external linter results
//! - `GET  /indexes/{id}/graph`                 KG graph
//! - `GET  /indexes/{id}/entities`              flat KG node list
//! - `GET  /indexes/{id}/clusters`              k-means concept clusters
//! - `GET  /indexes/{id}/ner`                   named-entity extraction
//! - `POST /indexes/{id}/scip`                  SCIP protobuf ingest
//! - `POST /review`                             diff review
//! - `POST /review/github-pr`                   GitHub PR review
//! - `POST /analyze/deep`                       LLM narrative pass
//! - `POST /webhooks/github`                    GitHub event webhook
//! - `GET  /facts`                              list / filter facts
//! - `POST /facts`                              upsert a fact
//! - `DELETE /facts/{id}`                       delete a fact
//!
//! Test: `cargo test -p trusty-analyze` boots the router with a stub
//! search client and exercises every route end-to-end.

mod diagnostics_dispatch;
mod ui;

pub mod events;
pub(crate) mod handlers;
pub mod routes;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_review;

// Re-export the public API so callers can write `use crate::service::…`
// without knowing which submodule owns each item.
pub use events::{AnalyzerAppState, AnalyzerEvent, DEFAULT_PORT};
pub use routes::{build_router, serve};

/// Re-export so the binary can construct facts via the same path.
pub use crate::types::FactRecord as PublicFactRecord;
