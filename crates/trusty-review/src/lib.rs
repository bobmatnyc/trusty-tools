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
// Why: coverage ingestion and verdict-gating (issue #1014).  Always compiled —
// no feature gate — because `CoveragePolicy` is a pure data type used by
// `ReviewConfig` and the runner in all modes.  The feature is off by default
// (opt-in via `TRUSTY_REVIEW_COVERAGE_ENABLED=true`).
pub mod coverage;
// Why: the voice module holds the 3-layer prompt composition types and logic
// (stock → principles → voice).  Always compiled — no feature gate — because
// VoiceConfig is a pure data type used by the prompt builder in all modes.
pub mod voice;
// Why: Phase 1 (#582) adds live posting, which needs a durable cross-process
// dedup claim store (redb) and an in-process in-flight guard.  These storage
// and concurrency concerns live in their own module, separate from the
// pipeline.  Always compiled — no feature gate (no axum/HTTP dependency).
pub mod store;
// Why: longitudinal contributor profiling (epic #558) requires a dedicated
// module for data models, identity resolution, period assembly, and diff
// sampling.  Gated behind the `profile` feature because it pulls in tga and
// rusqlite (heavy deps with vendored C libraries) that users who only need the
// core review pipeline should not have to compile.
#[cfg(feature = "profile")]
pub mod profile;

// Why: the service module is gated behind `http-server` so library consumers
// that only need the pipeline (CLI one-shot, unit tests) do not pull in the
// full axum + tower-http stack.
#[cfg(feature = "http-server")]
pub mod service;

// Why: the MCP module is gated behind the `mcp` feature so library consumers
// that do not need the stdio JSON-RPC loop do not pull in trusty-common's MCP
// primitives.  Enabled by default in the binary so `serve --stdio` is always
// available.  The `mcp` feature implies `http-server` (AppState is defined there).
#[cfg(feature = "mcp")]
pub mod mcp;
