//! The `server` feature: `secrets.*` on an on-demand Unix socket (S2, #9065).
//!
//! Why: owner ruling 24 — trusty-secrets serves its own socket; the tm daemon
//! and the console are clients. Ruling 28 — the socket spawns on first call
//! and exits when idle, so nothing is resident at rest and there is no launchd
//! job. Ruling 31 fixes the path (`~/.trusty-tools/trusty-secrets/secrets.sock`,
//! parent 0700) and the 60 s idle exit. Ruling 32 — the UDS stack is
//! trusty-common's.
//! What, in request order:
//! - [`settings`] — socket, index and config paths, and the idle window.
//! - `router` — bind, uid-checked serve loop, unlink on exit.
//! - [`project`] — a request's project directory to scopes and a backend.
//! - `methods` and `doctor` — the six method bodies. None returns a secret
//!   value.
//! - [`audit`] and `gate` — the credential access audit trail (#4567):
//!   one record per `set`/`delete` call, per `copy` key, per denied `list`.
//! - [`errors`] — the fixed error text every failure is reported with.
//! - [`client`] — the minimal spawn-on-first-call helper.
//!
//! `router` and `methods` are private (#9073): S8 changes the method table
//! and the body signature, so only the re-exports below are public.
//!
//! No item here logs or formats a request, a response, or a value.
//! Test: `server_tests.rs` beside this module (real sockets in temp dirs,
//! `MemoryBackend`), and `tests/on_demand_server.rs` (the real binary).
//!
//! Governing document: DOC-74 §15.2, §15.6
//! (`docs/specs/DOC-74-secrets-integration.md`).

pub mod audit;
pub mod client;
// #7524 P2-M1: one table for the server's deadline and the client's wait.
mod deadline;
mod doctor;
pub mod errors;
mod gate;
mod methods;
pub mod project;
mod router;
pub mod settings;
mod tools;

pub use audit::{AUDIT_STREAM, AuditDecision, AuditMethod, AuditReason, AuditRecord, AuditStream};
pub use client::{ClientError, OnDemandSecrets, RpcFailure, SECRETS_EXTERNAL_ENV, SECRETS_SERVICE};
pub use doctor::{
    BackendStatus, DOCTOR, DoctorResponse, HeadlessReadiness, StoragePosture, Unavailable,
};
pub use errors::ErrorKind;
pub use methods::PROJECT_FIELD;
pub use project::{PROJECT_CONFIG_SUBPATH, ProjectContext};
pub use router::{
    BackendFactory, ServeError, ServeExit, StartEnv, State, backends_for, default_backends, serve,
    serve_with,
};
pub use settings::{
    AUDIT_LOG_SUBPATH, DEFAULT_AUDIT_MAX_BYTES, DEFAULT_IDLE_TIMEOUT, IDLE_TIMEOUT_ENV,
    INDEX_DIR_ENV, SOCKET_ENV, SOCKET_SUBPATH, ServerSettings, SettingsError, template_root_beside,
};
pub use tools::DetectedTool;

#[cfg(test)]
#[path = "server_tests.rs"]
mod tests;
