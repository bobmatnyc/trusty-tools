//! `trusty-review` — fast local PR-review service.
//!
//! Why: orchestrates LLM-backed code review as a standalone crate within the
//! trusty-tools workspace, consuming trusty-search (context retrieval) and an
//! LLM provider (OpenRouter or Bedrock) to produce structured review verdicts.
//!
//! What: exposes the `config`, `llm`, and `models` modules that form the
//! Stage-1 foundation. Later stages add `pipeline`, `diff`, `integrations`,
//! `store`, `service`, and `cli` modules.
//!
//! Test: each public module carries its own unit tests; see each submodule.

pub mod config;
pub mod llm;
pub mod models;
