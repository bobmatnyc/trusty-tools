//! The minimal on-demand client: start the server if absent, make one call.
//!
//! Why: owner ruling 31 — clients spawn the server through trusty-common's
//! on-demand machinery, and nothing starts it at login. trusty-common's
//! `uds::on_demand` module is hard-wired to `trusty-analyze`, so this applies
//! the same mechanism it wraps — `UdsServiceSupervisor` in detached mode — to
//! `trusty-secrets`. The full Rust client is S9; this is the smallest helper
//! the tm daemon and the console bridge can call.
//! What: [`OnDemandSecrets`]: `ensure_running` (probe, spawn
//! `trusty-secrets serve --socket <path>` when nothing answers, wait for the
//! bind) and `call` (one framed JSON-RPC request; a dial failure re-runs
//! `ensure_running` once, since a server that idled out between the two never
//! saw the request).
//! Test: `on_demand_client_spawns_the_server_and_it_exits_when_idle` in
//! `tests/on_demand_server.rs`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};
use trusty_common::uds::server::{JSONRPC_VERSION, RpcError, RpcResponse};
use trusty_common::uds::{
    ServiceTimeouts, SpawnSpec, SupervisorConfig, SupervisorError, UdsRpcError,
    UdsServiceSupervisor, send_framed_request,
};

use super::settings::{SERVE_SUBCOMMAND, SOCKET_SUBPATH};

/// Binary and service name.
pub const SECRETS_SERVICE: &str = "trusty-secrets";

/// Set to `1` to make clients dial whatever serves the socket and never spawn.
pub const SECRETS_EXTERNAL_ENV: &str = "TRUSTY_SECRETS_EXTERNAL";

/// Spawn probe 10 s; the server holds no write buffer, so a 1 s flush and
/// 3 s SIGTERM patience cover the spawn-failure kill.
const TIMEOUTS: ServiceTimeouts = ServiceTimeouts::new(
    Duration::from_secs(10),
    Duration::from_secs(1),
    Duration::from_secs(3),
);

/// Budget for one request, connect included.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// A client call that did not produce a result.
///
/// What: the server's error arrives as [`ClientError::Rpc`], whose message
/// is the server's fixed text.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// `$HOME` is unknown, so the default socket path is too.
    #[error("home directory is unavailable")]
    HomeUnavailable,
    /// The server could not be started.
    #[error(transparent)]
    Spawn(#[from] SupervisorError),
    /// The request or response did not cross the socket.
    #[error(transparent)]
    Transport(#[from] UdsRpcError),
    /// The server answered with an error.
    #[error("{0}")]
    Rpc(RpcError),
    /// The server answered with neither a result nor an error.
    #[error("the server answered without a result")]
    EmptyResponse,
}

/// Starts `trusty-secrets` when nothing serves its socket, and calls it.
///
/// Why: one handle per process, so the supervisor's spawn gate serialises
/// concurrent callers; across processes `bind_singleton_hardened` in the
/// child decides the winner.
/// What: see the module docs.
/// Test: `on_demand_client_spawns_the_server_and_it_exits_when_idle`.
#[derive(Debug)]
pub struct OnDemandSecrets {
    supervisor: UdsServiceSupervisor,
    socket: PathBuf,
    program: Option<PathBuf>,
    server_args: Vec<OsString>,
}

impl OnDemandSecrets {
    /// A handle for `~/.trusty-tools/trusty-secrets/secrets.sock`.
    ///
    /// # Errors
    ///
    /// [`ClientError::HomeUnavailable`].
    pub fn new() -> Result<Self, ClientError> {
        let home = dirs::home_dir().ok_or(ClientError::HomeUnavailable)?;
        Ok(Self::at(home.join(SOCKET_SUBPATH)))
    }

    /// A handle for an explicit socket path, e.g. a test's temp dir.
    pub fn at(socket: impl Into<PathBuf>) -> Self {
        Self {
            supervisor: UdsServiceSupervisor::new(
                SupervisorConfig::new(SECRETS_SERVICE, 1, TIMEOUTS)
                    .with_external_env(SECRETS_EXTERNAL_ENV)
                    .with_detached(true),
            ),
            socket: socket.into(),
            program: None,
            server_args: Vec::new(),
        }
    }

    /// Spawn this executable instead of resolving `trusty-secrets` on `PATH`.
    #[must_use]
    pub fn with_program(mut self, program: impl Into<PathBuf>) -> Self {
        self.program = Some(program.into());
        self
    }

    /// Extra `serve` flags for a spawned server, e.g. `--index-dir`.
    #[must_use]
    pub fn with_server_args(mut self, args: impl IntoIterator<Item = impl Into<OsString>>) -> Self {
        self.server_args = args.into_iter().map(Into::into).collect();
        self
    }

    /// The socket this handle dials.
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// Return the socket path with a server answering on it.
    ///
    /// What: trusty-common's `ensure_running` — returns at once when the
    /// socket answers; otherwise spawns `serve --socket <path>` plus the
    /// extra args, detached into its own session, with stderr appended to
    /// `trusty-secrets.stderr.log` beside the socket.
    ///
    /// # Errors
    ///
    /// [`ClientError::Spawn`]: no binary, a refused spawn, a child that
    /// exited before binding, or no socket inside the spawn budget.
    pub async fn ensure_running(&self) -> Result<PathBuf, ClientError> {
        let path = self
            .supervisor
            .ensure_running(SECRETS_SERVICE, &self.socket, || {
                let program = match &self.program {
                    Some(program) => program.clone(),
                    None => trusty_common::bin_resolve::resolve_binary(SECRETS_SERVICE)
                        .ok_or("trusty-secrets is not installed")?,
                };
                let mut spec = SpawnSpec::new(program)
                    .arg(SERVE_SUBCOMMAND)
                    .arg("--socket")
                    .arg(&self.socket);
                for arg in &self.server_args {
                    spec = spec.arg(arg.clone());
                }
                if let Some(dir) = self.socket.parent() {
                    spec = spec
                        .create_dir(dir)
                        .stderr_to(dir.join(format!("{SECRETS_SERVICE}.stderr.log")));
                }
                Ok(spec)
            })
            .await?;
        Ok(path)
    }

    /// Call `method` with `params`, starting the server first if needed.
    ///
    /// # Errors
    ///
    /// [`ClientError`]; the server's own refusals are [`ClientError::Rpc`].
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, ClientError> {
        let request = json!({
            "jsonrpc": JSONRPC_VERSION,
            "id": 1,
            "method": method,
            "params": params,
        });
        let path = self.ensure_running().await?;
        let response: RpcResponse = match send_framed_request(&path, &request, CALL_TIMEOUT).await {
            // Never connected, so never sent: safe to re-spawn and resend.
            Err(UdsRpcError::Dial { .. } | UdsRpcError::ConnectRetriesExhausted { .. }) => {
                let path = self.ensure_running().await?;
                send_framed_request(&path, &request, CALL_TIMEOUT).await?
            }
            other => other?,
        };
        match (response.result, response.error) {
            (_, Some(error)) => Err(ClientError::Rpc(error)),
            (Some(result), None) => Ok(result),
            (None, None) => Err(ClientError::EmptyResponse),
        }
    }
}
