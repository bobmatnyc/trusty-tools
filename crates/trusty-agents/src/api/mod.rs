//! HTTP API + JSON response envelope for PM/workflow output (#151).
//!
//! Why: Uniform JSON output lets external clients (a thin `ompm` CLI, a
//! future GUI, CI pipelines) consume PM results without parsing free-form
//! text. The envelope also carries per-phase perf + file lists that were
//! previously only reachable via `docs/performance/runs/*.json`.
//! What: `types` defines the wire shape. `builder` projects in-process
//! `WorkflowContext` + `PerfRecord` into a `PmResponse`. `server` (Phase 2)
//! exposes an axum HTTP API on top of those primitives.
//! Test: Each submodule carries its own unit tests.

pub mod builder;
pub mod server;
pub mod types;

#[allow(unused_imports)]
pub use types::{PhaseProgress, PhaseResult, PmMetadata, PmResponse, PmResponseType, PmStatus};
