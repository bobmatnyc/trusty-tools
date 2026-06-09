//! HTTP route handler submodules.
//!
//! Why: Groups handlers by feature area so each file stays well under the
//! 500-line cap and readers can find handlers by domain without scanning the
//! entire service module.
//!
//! What: Re-exports the five handler modules:
//! - `analysis` — complexity hotspots, smells, quality, refactor, diagnostics
//! - `graph` — KG graph/entities, clustering, NER, SCIP ingest
//! - `facts` — CRUD for the FactStore knowledge triples
//! - `review` — diff review, GitHub PR review, webhooks
//! - `deep` — LLM deep-analysis pass (`POST /analyze/deep`)
//!
//! Test: All handler tests live in `service/tests.rs` and `service/tests_review.rs`.

pub mod analysis;
pub mod deep;
pub mod facts;
pub mod graph;
pub mod review;
