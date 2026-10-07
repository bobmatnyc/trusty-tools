//! tm's shared client seam for the trusty-secrets on-demand socket (#7522).
//!
//! Why: owner ruling 34 — the `tm secrets` CLI (#7521) and the daemon's
//! `secrets_get_ref` MCP tool are both clients of the trusty-secrets socket.
//! The CLI lives in the `tm` bin and the MCP catalog in this library, so the
//! request shape and the fixed error text both callers rely on live here once.
//! What: [`with_project`](crate::secrets_client::with_project) folds the
//! project directory into a request's params;
//! [`describe`](crate::secrets_client::describe) renders a
//! [`ClientError`](trusty_secrets::server::ClientError) as text that cannot
//! carry a request field;
//! [`default_client`](crate::secrets_client::default_client) is the
//! process-wide handle for the default socket;
//! [`get_ref()`](crate::secrets_client::get_ref()) backs the MCP tool. Nothing
//! here logs a value.
//! Test: `tests.rs` beside this module — the tool end to end through
//! `crate::mcp::dispatch` over a real socket, plus the helpers' fixed text.
//! The CLI's own suite (`bin/tm/commands/secrets/tests.rs`) covers the CLI.
//!
//! Governing document: DOC-74 §10.2, §15.2
//! (`docs/specs/DOC-74-secrets-integration.md`).

use std::path::Path;
use std::sync::OnceLock;

use serde_json::{Map, Value};
use trusty_secrets::server::{ClientError, OnDemandSecrets, PROJECT_FIELD};

mod get_ref;

pub use get_ref::{GetRefError, RefAnswer, get_ref};

/// `params` with the `project` field set to `project`.
///
/// Why: every `secrets.*` method takes the project directory in the same
/// flat params object (trusty-secrets `server::methods`).
/// What: `params` must be an object or null; anything else is replaced by an
/// empty object. `None` when `project` is not UTF-8.
/// Test: `with_project_folds_the_directory_into_the_params`.
pub fn with_project(project: &Path, params: Value) -> Option<Value> {
    let project = project.to_str()?;
    let mut fields = match params {
        Value::Object(fields) => fields,
        _ => Map::new(),
    };
    fields.insert(PROJECT_FIELD.to_owned(), Value::String(project.to_owned()));
    Some(Value::Object(fields))
}

/// A client failure as `"<prefix>: <fixed text>"`, carrying no request field.
///
/// Why: the server already answers with fixed text (`ErrorKind::to_rpc`); a
/// transport error's detail is a library message, so it is dropped.
/// What: the server's own message, a spawn failure, or a fixed sentence
/// naming the socket path.
/// Test: `describe_keeps_the_cli_text_and_drops_transport_detail`.
pub fn describe(prefix: &str, error: &ClientError, socket: &Path) -> String {
    let socket = socket.display();
    match error {
        ClientError::Rpc(e) => format!("{prefix}: {}", e.message),
        ClientError::Spawn(e) => format!("{prefix}: cannot start trusty-secrets: {e}"),
        ClientError::Transport(_) => {
            format!("{prefix}: the request did not cross the trusty-secrets socket {socket}")
        }
        ClientError::HomeUnavailable => format!("{prefix}: the home directory is unavailable"),
        ClientError::EmptyResponse => {
            format!("{prefix}: trusty-secrets at {socket} answered without a result")
        }
        // #7521: `ClientError` is `#[non_exhaustive]`; a later variant gets
        // fixed text, never its own detail.
        _ => format!("{prefix}: trusty-secrets at {socket} failed"),
    }
}

/// The process-wide client for `~/.trusty-tools/trusty-secrets/secrets.sock`.
///
/// Why: trusty-secrets' client serialises concurrent spawns per handle, so
/// the daemon keeps one handle rather than one per MCP call.
/// What: built on first use and kept for the process.
///
/// # Errors
///
/// [`ClientError::HomeUnavailable`].
pub fn default_client() -> Result<&'static OnDemandSecrets, ClientError> {
    static CLIENT: OnceLock<OnDemandSecrets> = OnceLock::new();
    if let Some(client) = CLIENT.get() {
        return Ok(client);
    }
    let client = OnDemandSecrets::new()?;
    Ok(CLIENT.get_or_init(|| client))
}

#[cfg(test)]
mod tests;
