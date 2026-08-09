//! `trusty-analyze` — sidecar code-analysis daemon for trusty-search.
//!
//! # What it does
//!
//! `trusty-analyze` is an HTTP daemon and MCP server that fetches indexed code
//! corpora from a running [trusty-search](https://github.com/bobmatnyc/trusty-tools)
//! instance, performs static analysis, and exposes results on port 7879. It
//! supports cyclomatic / cognitive complexity, code-smell detection, quality-grade
//! aggregation (A–F), git-blame temporal decay, k-means concept clustering
//! (hashed bag-of-words), a facts store (redb), SCIP protobuf
//! ingest for LSP-quality symbol data, and an optional deep-analysis LLM pass
//! (OpenRouter or AWS Bedrock). Every HTTP endpoint has an MCP tool equivalent.
//!
//! # Prerequisites — REQUIRED before starting
//!
//! **`trusty-analyze` requires a running `trusty-search` daemon.** The analyzer
//! performs a startup health check against `http://127.0.0.1:7878/health` (or the
//! URL given by `--search-url`) and exits with code 1 if that check fails. There
//! is no standalone or offline mode. Start `trusty-search` first:
//!
//! ```text
//! trusty-search daemon   # or: trusty-search start
//! ```
//!
//! See the [trusty-search install guide](https://github.com/bobmatnyc/trusty-tools)
//! for setup instructions.
//!
//! # Installation
//!
//! **From source (recommended — `cargo install --git`):**
//!
//! ```text
//! cargo install --git https://github.com/bobmatnyc/trusty-tools trusty-analyze --locked
//! ```
//!
//! The build links no ONNX Runtime and needs no model cache. #5067 removed the
//! `bundled-ort` / `load-dynamic` / `cuda` features along with the neural
//! clustering embedder they existed for, so there is no per-glibc install
//! variant and no `ORT_DYLIB_PATH` to set.
//!
//! **Prebuilt binaries** for macOS arm64 and Linux x86_64 are published on the
//! [GitHub Releases page](https://github.com/bobmatnyc/trusty-tools/releases)
//! under tags of the form `trusty-analyze-v<version>`.
//!
//! # Usage
//!
//! ```text
//! # Start trusty-search first (hard dependency)
//! trusty-search daemon
//!
//! # Then start the analyzer
//! trusty-analyze serve --search-url http://127.0.0.1:7878
//!
//! # Analyze an index and view complexity hotspots
//! trusty-analyze analyze <index-id> --top-k 20
//!
//! # Check liveness
//! trusty-analyze health
//! ```
//!
//! # Further reading
//!
//! See the [README](https://github.com/bobmatnyc/trusty-tools/blob/main/crates/trusty-analyze/README.md)
//! for the full API reference, MCP tool list, feature-flag documentation, and
//! Claude Code integration instructions.

pub mod core;
pub mod embedder;
pub mod lang;
pub mod mcp;
// Why (issue #249): the `service` module is the axum HTTP daemon surface and
// only compiles when the `http-server` feature is enabled. Gating it keeps
// library consumers that only need the analysis core / dispatcher free of the
// axum + tower-http stack.
#[cfg(feature = "http-server")]
pub mod service;
pub mod types;
// #5182: the console->target webhook UDS listener. No feature gate — it pulls
// in no HTTP stack, and the whole point is that the socket exists without the
// service running resident (ADR-0034 §1, milestone criterion (c)).
pub mod webhook_listener;
