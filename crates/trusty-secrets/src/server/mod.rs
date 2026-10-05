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
//! - [`router`] — bind, uid-checked serve loop, unlink on exit.
//! - [`project`] — a request's project directory to scopes and a backend.
//! - [`methods`] — the six method bodies. None returns a secret value.
//! - [`errors`] — the fixed error text every failure is reported with.
//! - [`client`] — the minimal spawn-on-first-call helper.
//!
//! No item here logs or formats a request, a response, or a value.
//! Test: `server_tests.rs` beside this module (real sockets in temp dirs,
//! `MemoryBackend`), and `tests/on_demand_server.rs` (the real binary).
//!
//! Governing document: DOC-74 §15.2, §15.6
//! (`docs/specs/DOC-74-secrets-integration.md`).

pub mod client;
pub mod errors;
pub mod methods;
pub mod project;
pub mod router;
pub mod settings;

pub use client::{ClientError, OnDemandSecrets, SECRETS_EXTERNAL_ENV, SECRETS_SERVICE};
pub use errors::ErrorKind;
pub use methods::{BackendStatus, DOCTOR, DoctorResponse, PROJECT_FIELD};
pub use project::{PROJECT_CONFIG_SUBPATH, ProjectContext};
pub use router::{BackendFactory, ServeError, State, build_router, default_backends, serve};
pub use settings::{
    DEFAULT_IDLE_TIMEOUT, IDLE_TIMEOUT_ENV, INDEX_DIR_ENV, SOCKET_ENV, SOCKET_SUBPATH,
    ServerSettings, SettingsError,
};

#[cfg(test)]
#[path = "server_tests.rs"]
mod tests;
