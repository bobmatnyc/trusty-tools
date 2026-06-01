//! `trusty-review` — fast local PR-review service.
//!
//! Why: orchestrates LLM-backed code review as a standalone crate within the
//! trusty-tools workspace, consuming trusty-search (context retrieval) and an
//! LLM provider (OpenRouter or Bedrock) to produce structured review verdicts.
//!
//! What: exposes `config`, `llm`, `models`, `integrations`, and `pipeline`.
//! Stage-1 delivered config + LLM provider abstraction; Stage-2 adds the
//! integration clients (GitHub App auth, trusty-search/analyze HTTP clients);
//! Stage-3 adds the MVP review pipeline and the `run`/`compare` CLI commands;
//! Stage-4 adds the `service` module — axum HTTP server with /health, /status,
//! /review, and /pr/github/webhook (gated behind the `http-server` feature).
//!
//! Test: each public module carries its own unit tests; see each submodule.

pub mod config;
pub mod integrations;
pub mod llm;
pub mod models;
pub mod pipeline;
// Why: longitudinal contributor profiling (epic #558) requires a dedicated
// module for data models, identity resolution, period assembly, and diff
// sampling.  It is always compiled (no feature gate) because these types are
// pure data structures and read-only DB queries with no HTTP / axum dep.
pub mod profile;

// Why: the service module is gated behind `http-server` so library consumers
// that only need the pipeline (CLI one-shot, unit tests) do not pull in the
// full axum + tower-http stack.
#[cfg(feature = "http-server")]
pub mod service;
