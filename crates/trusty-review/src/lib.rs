//! `trusty-review` — fast local PR-review service.
//!
//! Why: orchestrates LLM-backed code review as a standalone crate within the
//! trusty-tools workspace, consuming trusty-search (context retrieval) and an
//! LLM provider (OpenRouter or Bedrock) to produce structured review verdicts.
//!
//! What: exposes `config`, `llm`, `models`, `integrations`, and `pipeline`.
//! Stage-1 delivered config + LLM provider abstraction; Stage-2 adds the
//! integration clients (GitHub App auth, trusty-search/analyze HTTP clients);
//! Stage-3 adds the MVP review pipeline and the `run`/`compare` CLI commands.
//!
//! Test: each public module carries its own unit tests; see each submodule.

pub mod config;
pub mod integrations;
pub mod llm;
pub mod models;
pub mod pipeline;
