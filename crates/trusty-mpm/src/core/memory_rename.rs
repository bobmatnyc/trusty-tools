//! `tm memory rename <old> <new> [--replace-empty]` — trusty-memory's
//! `palace_rename` over the daemon socket (#9544).
//!
//! Why: moving a palace to a new id by hand leaves the old id dead and races
//! every open of either id; trusty-memory's `palace_rename` does the move
//! safely, and this is the operator's way to call it without an MCP session.
//! What: [`rename_palace_at`] runs the protocol handshake
//! (`ensure_memory_protocol_at`), then calls `palace_rename` by raw name with
//! `call_memory_tool_at_with_timeout` — not `memory_verbs::call_method`, which
//! flattens the daemon's error code into text. [`MemoryRenameError`] keeps the
//! two codes an operator acts on: -32601 (the daemon predates the method) and
//! -32006 (a refusal, with the daemon's reason).
//! Test: `memory_rename_tests.rs`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};
use trusty_common::memory_rpc::{
    DEFAULT_TIMEOUT, MemoryRpcError, call_memory_tool_at_with_timeout, ensure_memory_protocol_at,
    resolve_memory_socket,
};
use trusty_common::uds::server::CODE_METHOD_NOT_FOUND;

/// The trusty-memory method `tm memory rename` calls.
pub const RENAME_METHOD: &str = "palace_rename";

/// trusty-memory's "well formed and refused" code (its `transport::CODE_REFUSED`).
pub const CODE_REFUSED: i64 = -32006;

/// Budget for the rename call: it waits on both palaces' write locks and opens
/// the palace twice to verify its counts, so the 5 s default is too short for
/// a large palace.
pub const RENAME_TIMEOUT: Duration = Duration::from_secs(120);

/// Why `tm memory rename` did not rename.
///
/// Why: "restart the daemon on a newer release", "read the refusal" and "the
/// daemon is not reachable" are different next moves, so each is a variant.
/// Test: `rename_surfaces_refusal_32006_message`,
/// `rename_against_a_daemon_that_answers_32601_says_it_predates_palace_rename`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MemoryRenameError {
    /// The trusty-memory socket path could not be derived.
    #[error("could not resolve the trusty-memory socket: {detail}")]
    Socket {
        /// What resolution reported.
        detail: String,
    },
    /// The protocol handshake refused the daemon.
    #[error("{detail}")]
    Protocol {
        /// The handshake's own message.
        detail: String,
    },
    /// The daemon answered -32601: it has no `palace_rename`.
    #[error(
        "the trusty-memory daemon at {socket} predates palace_rename; restart it on a \
         release that has it, then retry"
    )]
    Predates {
        /// The socket that was dialled.
        socket: String,
    },
    /// The daemon refused the rename (-32006); nothing was renamed.
    #[error("palace rename refused: {message}")]
    Refused {
        /// The daemon's reason.
        message: String,
    },
    /// The daemon answered with another error code.
    #[error("palace_rename failed ({code}): {message}")]
    Daemon {
        /// The daemon's JSON-RPC code.
        code: i64,
        /// The daemon's message.
        message: String,
    },
    /// Nothing answered, or the answer could not be read.
    #[error("trusty-memory did not answer palace_rename at {socket}: {detail}")]
    Call {
        /// The socket that was dialled.
        socket: String,
        /// What the transport reported.
        detail: String,
    },
}

/// The `palace_rename` arguments, keyed as trusty-memory's tool schema keys them.
pub fn rename_arguments(old: &str, new: &str, replace_empty: bool) -> Value {
    json!({"palace_id": old, "new_id": new, "replace_empty": replace_empty})
}

/// [`rename_palace_at`] on `socket`, else the derived trusty-memory socket.
pub async fn rename_palace(
    old: &str,
    new: &str,
    replace_empty: bool,
    socket: Option<PathBuf>,
) -> Result<Value, MemoryRenameError> {
    let socket = match socket {
        Some(socket) => socket,
        None => resolve_memory_socket().map_err(|e| MemoryRenameError::Socket {
            detail: format!("{e:#}"),
        })?,
    };
    rename_palace_at(&socket, old, new, replace_empty).await
}

/// Rename palace `old` to `new` through the daemon at `socket`.
///
/// Why: see the module docs.
/// What: the handshake first, so a daemon outside the supported protocol
/// range is refused before any write; then one `palace_rename` call. Returns
/// the daemon's success payload.
/// Test: `rename_calls_ensure_memory_protocol_at_first`,
/// `rename_arguments_use_the_schema_keys`.
pub async fn rename_palace_at(
    socket: &Path,
    old: &str,
    new: &str,
    replace_empty: bool,
) -> Result<Value, MemoryRenameError> {
    ensure_memory_protocol_at(socket, DEFAULT_TIMEOUT)
        .await
        .map_err(|e| MemoryRenameError::Protocol {
            detail: e.to_string(),
        })?;
    call_memory_tool_at_with_timeout(
        socket,
        RENAME_METHOD,
        rename_arguments(old, new, replace_empty),
        RENAME_TIMEOUT,
    )
    .await
    .map_err(|e| classify(socket, e))
}

/// Map a failed call onto the variant an operator acts on.
fn classify(socket: &Path, e: anyhow::Error) -> MemoryRenameError {
    match e.downcast_ref::<MemoryRpcError>() {
        Some(rpc) if rpc.code == CODE_METHOD_NOT_FOUND => MemoryRenameError::Predates {
            socket: socket.display().to_string(),
        },
        Some(rpc) if rpc.code == CODE_REFUSED => MemoryRenameError::Refused {
            message: rpc.message.clone(),
        },
        Some(rpc) => MemoryRenameError::Daemon {
            code: rpc.code,
            message: rpc.message.clone(),
        },
        None => MemoryRenameError::Call {
            socket: socket.display().to_string(),
            detail: format!("{e:#}"),
        },
    }
}

#[cfg(test)]
#[path = "memory_rename_tests.rs"]
mod tests;
