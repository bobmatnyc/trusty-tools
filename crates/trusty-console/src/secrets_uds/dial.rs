//! The console's dial to trusty-secrets: start it if absent, make one call
//! (#9066, Architect ruling Q1).
//!
//! Why: trusty-common's `uds::on_demand` serves trusty-analyze only, and
//! trusty-secrets' own `OnDemandSecrets` sits behind its `server` feature,
//! which the console does not link (owner ruling Df0). So this file applies
//! the same mechanism — `UdsServiceSupervisor` in detached mode plus
//! `send_framed_request` — with the console's own constants.
//! What: [`SecretsDial`]: `ensure_running` on every call (Architect Q3a),
//! one framed JSON-RPC request, and a [`DialError`] that keeps "could not
//! start or reach" (503) apart from "reached, then failed" (502).
//! Test: `dial_reports_a_missing_binary_as_unavailable`,
//! `dial_in_external_mode_with_no_socket_is_unavailable`,
//! `dial_reports_a_hangup_as_transport`.

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};
use trusty_common::uds::server::{JSONRPC_VERSION, RpcResponse};
use trusty_common::uds::{
    ServiceTimeouts, SpawnSpec, SupervisorConfig, UdsRpcError, UdsServiceSupervisor,
    send_framed_request,
};

/// Binary and service name of the secrets server.
pub const SECRETS_SERVICE: &str = "trusty-secrets";
/// Set to `1` to dial whatever serves the socket and never spawn.
pub const SECRETS_EXTERNAL_ENV: &str = "TRUSTY_SECRETS_EXTERNAL";
/// The socket under `$HOME` (trusty-secrets `settings::SOCKET_SUBPATH`).
pub const SOCKET_SUBPATH: &str = ".trusty-tools/trusty-secrets/secrets.sock";
/// The server's subcommand (trusty-secrets `settings::SERVE_SUBCOMMAND`).
pub const SERVE_SUBCOMMAND: &str = "serve";
/// The server's deadline for a method that writes through a backend.
pub const WRITE_DEADLINE: Duration = Duration::from_secs(120);
/// The server's deadline for every other method.
pub const READ_DEADLINE: Duration = Duration::from_secs(15);
/// How much longer than the server's deadline a client waits.
pub const CLIENT_MARGIN: Duration = Duration::from_secs(15);

/// Spawn probe 10 s, flush 1 s, SIGTERM patience 3 s, as the server's own client.
const TIMEOUTS: ServiceTimeouts = ServiceTimeouts::new(
    Duration::from_secs(10),
    Duration::from_secs(1),
    Duration::from_secs(3),
);

/// How long the console waits for the reply to bridged `method`.
///
/// What: the server's deadline for the direct method plus the margin, so a
/// reply the server sends is never dropped as a timeout. `list` reads; the
/// other four bridged methods write.
pub fn client_wait(method: &str) -> Duration {
    use trusty_secrets::api::bridge::method;
    let deadline = if method == method::LIST {
        READ_DEADLINE
    } else {
        WRITE_DEADLINE
    };
    deadline + CLIENT_MARGIN
}

/// Why a call produced no result.
#[derive(Debug)]
pub enum DialError {
    /// The server could not be started, or is absent in external mode.
    Unavailable,
    /// The server was reached but the exchange did not complete.
    Transport,
    /// The server answered with an error; `kind` is its `error.data.kind`.
    Rpc {
        /// The wire kind, when the error carried one.
        kind: Option<String>,
    },
}

/// Starts `trusty-secrets` when nothing serves its socket, and calls it.
///
/// Why: one handle per console, so the supervisor's spawn gate serialises
/// concurrent requests.
/// What: see the module docs.
/// Test: the `dial_*` tests in `tests.rs`.
#[derive(Debug)]
pub struct SecretsDial {
    supervisor: UdsServiceSupervisor,
    socket: PathBuf,
    program: Option<PathBuf>,
    external: bool,
}

impl SecretsDial {
    /// A dial for `socket`; external mode is read from the environment once.
    pub fn new(socket: PathBuf) -> Self {
        let config = SupervisorConfig::new(SECRETS_SERVICE, 1, TIMEOUTS)
            .with_external_env(SECRETS_EXTERNAL_ENV)
            .with_detached(true);
        let external = config.external_mode_enabled();
        Self {
            supervisor: UdsServiceSupervisor::new(config),
            socket,
            program: None,
            external,
        }
    }

    /// Spawn this executable instead of resolving `trusty-secrets` on `PATH`.
    #[must_use]
    pub fn with_program(mut self, program: impl Into<PathBuf>) -> Self {
        self.program = Some(program.into());
        self
    }

    /// Never spawn: dial whatever serves the socket (tests; production reads
    /// [`SECRETS_EXTERNAL_ENV`]).
    #[must_use]
    pub fn with_external(mut self, external: bool) -> Self {
        self.external = external;
        self
    }

    /// The socket this dial uses.
    pub fn socket(&self) -> &std::path::Path {
        &self.socket
    }

    async fn ensure_running(&self) -> Result<PathBuf, DialError> {
        if self.external {
            return Ok(self.socket.clone());
        }
        self.supervisor
            .ensure_running(SECRETS_SERVICE, &self.socket, || {
                let program = match &self.program {
                    Some(program) => program.clone(),
                    None => trusty_common::bin_resolve::resolve_binary(SECRETS_SERVICE)
                        .ok_or("trusty-secrets is not installed")?,
                };
                let mut spec = SpawnSpec::new(program)
                    .arg(SERVE_SUBCOMMAND)
                    .arg("--socket")
                    .arg(OsString::from(self.socket.as_os_str()));
                if let Some(dir) = self.socket.parent() {
                    spec = spec
                        .create_dir(dir)
                        .stderr_to(dir.join(format!("{SECRETS_SERVICE}.stderr.log")));
                }
                Ok(spec)
            })
            .await
            // #9066 Q5: the cause is not relayed; the kind of failure is.
            .map_err(|_| DialError::Unavailable)
    }

    /// Call `method` with `params`, starting the server first if needed.
    ///
    /// What: a dial that never connected re-runs `ensure_running` once (a
    /// server that idled out never saw the request); a second dial failure
    /// is [`DialError::Unavailable`]. Any failure after connecting is
    /// [`DialError::Transport`].
    ///
    /// # Errors
    ///
    /// [`DialError`].
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, DialError> {
        let request = json!({
            "jsonrpc": JSONRPC_VERSION,
            "id": 1,
            "method": method,
            "params": params,
        });
        let wait = client_wait(method);
        let path = self.ensure_running().await?;
        let response: RpcResponse = match send_framed_request(&path, &request, wait).await {
            Err(e) if never_connected(&e) => {
                let path = self.ensure_running().await?;
                send_framed_request(&path, &request, wait)
                    .await
                    .map_err(classify)?
            }
            other => other.map_err(classify)?,
        };
        match (response.result, response.error) {
            (_, Some(error)) => Err(DialError::Rpc {
                kind: error
                    .data
                    .as_ref()
                    .and_then(|data| data.get("kind"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            }),
            (Some(result), None) => Ok(result),
            (None, None) => Err(DialError::Transport),
        }
    }
}

fn never_connected(error: &UdsRpcError) -> bool {
    matches!(
        error,
        UdsRpcError::Dial { .. } | UdsRpcError::ConnectRetriesExhausted { .. }
    )
}

fn classify(error: UdsRpcError) -> DialError {
    if never_connected(&error) {
        DialError::Unavailable
    } else {
        DialError::Transport
    }
}
