//! HTTP API + JSON response envelope for PM/workflow output (#151).
//!
//! Why: Uniform JSON output lets external clients (a thin `ompm` CLI, a
//! future GUI, CI pipelines) consume PM results without parsing free-form
//! text. The envelope also carries per-phase perf + file lists that were
//! previously only reachable via `docs/performance/runs/*.json`.
//! What: `types` defines the wire shape. `builder` projects in-process
//! `WorkflowContext` + `PerfRecord` into a `PmResponse`. `server` (Phase 2)
//! defines the axum router on top of those primitives; `uds` (#6433) is the
//! transport that carries it — a hardened Unix socket, not a TCP port.
//! Test: Each submodule carries its own unit tests.

pub mod builder;
pub mod server;
// #6433 slice 2: the `--serve`/`--api` daemon's transport — a hardened Unix
// socket in place of the loopback TCP listener (ADR-0032, ADR-0018).
pub mod uds;
pub mod types;
pub mod watchdog;

#[allow(unused_imports)]
pub use types::{PhaseProgress, PhaseResult, PmMetadata, PmResponse, PmResponseType, PmStatus};
