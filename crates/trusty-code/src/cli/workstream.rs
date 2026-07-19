//! `tcode workstream <action>` / `tcode ws <action>` (#3296, epic #3292).
//!
//! # Spec References
//!
//! - [`SPEC-WS-05~draft`](docs/specs/DOC-48-tcode-workstreams.md#SPEC-WS-05~draft)
//! - [`SPEC-WS-06~draft`](docs/specs/DOC-48-tcode-workstreams.md#SPEC-WS-06~draft)
//!
//! Why: the CLI-layer half of DOC-48 §5.4's `tcode workstream` family —
//! `create`/`get`/`list`/`activate`/`deactivate`/`close`, all thin JSON-RPC
//! clients over an ephemeral `tcode serve --stdio` child, matching every
//! other handler in this directory (`cli::session`/`attach`/`cancel`/
//! `transcript`). Unlike sessions, workstreams are DAEMON-PERSISTED (flat
//! JSON per project, `crate::workstreams::path`) — so, unlike
//! `cli::session::list`'s "only sees sessions from THIS invocation's daemon"
//! caveat, two SEPARATE `tcode workstream` invocations against the same
//! `--project` see the SAME workstreams, because the store file survives
//! between ephemeral daemon spawns.
//!
//! **`deactivate` takes no id (DOC-48 §5.4's CLI surface), but the RPC verb
//! it wraps does** (`workstream.deactivate{id}`, `crate::workstreams::protocol`)
//! — only the active workstream may ever be deactivated (§4.3), so this
//! handler resolves the id itself via one `workstream.list` call before
//! deactivating (see [`deactivate`]'s own docs).
//!
//! **`activate` does NOT start streaming events** (DOC-48 §5.4 says a CLI
//! client should auto-stream from the newly-active workstream): the
//! workstream-level SSE aggregation route (`GET /workstreams/{id}/events`,
//! issue #3297) is explicitly out of this ticket's scope per
//! `crate::workstreams::protocol`'s own module docs ("The CLI (#3296) and the
//! SSE aggregation route (#3297) remain out of scope") and isn't reachable
//! over the stdio JSON-RPC transport this CLI uses regardless (SSE is
//! HTTP-only). [`activate`] performs the activation call and reports the
//! outcome; wiring auto-attach is deferred until #3297 lands a route this
//! handler can actually call.
//!
//! **ID arguments require the full UUID.** DOC-48's RPC layer has no
//! prefix-matching support (`crate::workstreams::protocol::parse_id` is a
//! plain `Uuid::parse_str`), so building client-side "list, then match an
//! unambiguous prefix" resolution here would race against concurrent
//! creates/closes for a purely cosmetic convenience. Left as a documented
//! future nicety — [`crate::cli_client::render::render_workstream_table`]'s
//! displayed id prefix is DISPLAY ONLY.
//! What: [`list`] (table view), [`get`] (raw JSON view), [`create`],
//! [`activate`] (special-cases `-32008 active_conflict` into a human message
//! naming the active workstream and suggesting `--force`, via
//! [`explain_activation_error`]), [`deactivate`], [`close`].
//! Test: `tests::explain_activation_error_names_the_active_workstream_and_suggests_force`,
//! `tests::explain_activation_error_passes_through_other_errors`; the full
//! spawn+wire behaviour is covered by `tests/cli_e2e.rs`.

use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use serde_json::json;

use trusty_code::cli_client::render::{WorkstreamRow, render_workstream_table};
use trusty_code::cli_client::{RpcClientError, StdioRpcClient};

use super::tcode_exe;

/// `tcode workstream list [--include-closed] [--project P]`.
///
/// Why/What: see module docs. Renders
/// [`render_workstream_table`] over the daemon's `workstream.list`
/// response.
pub async fn list(project: &Path, include_closed: bool) -> Result<()> {
    let exe = tcode_exe::resolve()?;
    let mut client = StdioRpcClient::spawn(&exe, project)?;
    let result = client
        .call("workstream.list", json!({"include_closed": include_closed}))
        .await;
    client.shutdown(Duration::from_secs(5)).await?;
    let result = result?;

    let rows: Vec<WorkstreamRow> = serde_json::from_value(
        result
            .get("workstreams")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
    )
    .map_err(|e| anyhow::anyhow!("tcode workstream list: malformed daemon response: {e}"))?;
    println!("{}", render_workstream_table(&rows));
    Ok(())
}

/// `tcode workstream get <ID> [--project P]`.
///
/// Why/What: DOC-48 §5.4 specifies JSON output for `get` (unlike `list`'s
/// table); prints the daemon's raw `workstream.get` result pretty-printed
/// rather than re-shaping it, so every field the daemon returns is visible.
pub async fn get(project: &Path, id: &str) -> Result<()> {
    let exe = tcode_exe::resolve()?;
    let mut client = StdioRpcClient::spawn(&exe, project)?;
    let result = client.call("workstream.get", json!({"id": id})).await;
    client.shutdown(Duration::from_secs(5)).await?;
    let result = result?;
    println!(
        "{}",
        serde_json::to_string_pretty(&result)
            .map_err(|e| anyhow::anyhow!("tcode workstream get: failed to render JSON: {e}"))?
    );
    Ok(())
}

/// `tcode workstream create [--name NAME] [--project P]`.
///
/// Why/What: `name` is optional (DOC-48 §5.1: "Name is initially empty");
/// prints the newly-minted id so a script or the next command in a shell
/// session can capture it.
pub async fn create(project: &Path, name: Option<String>) -> Result<()> {
    let exe = tcode_exe::resolve()?;
    let mut client = StdioRpcClient::spawn(&exe, project)?;
    let result = client
        .call("workstream.create", json!({"name": name}))
        .await;
    client.shutdown(Duration::from_secs(5)).await?;
    let result = result?;
    let id = result.get("id").and_then(|v| v.as_str()).unwrap_or("?");
    println!("created workstream {id}");
    Ok(())
}

/// `tcode workstream activate <ID> [--force] [--project P]`.
///
/// Why/What: see module docs re: deferred auto-streaming. A plain success
/// reports the new active id (and the prior one, if this was a force
/// switch); an `ActiveConflict` (`-32008`) is translated by
/// [`explain_activation_error`] into a message naming the currently-active
/// workstream and suggesting `--force`, rather than surfacing the daemon's
/// generic `RpcClientError::Rpc` `Display` text.
pub async fn activate(project: &Path, id: &str, force: bool) -> Result<()> {
    let exe = tcode_exe::resolve()?;
    let mut client = StdioRpcClient::spawn(&exe, project)?;
    let result = client
        .call("workstream.activate", json!({"id": id, "force": force}))
        .await;
    client.shutdown(Duration::from_secs(5)).await?;

    let value = result.map_err(|e| explain_activation_error(e, id))?;
    match value.get("prior_id").and_then(|v| v.as_str()) {
        Some(prior_id) => println!("activated workstream {id} (switched from {prior_id})"),
        None => println!("activated workstream {id}"),
    }
    Ok(())
}

/// `tcode workstream deactivate [--project P]`.
///
/// Why/What: see module docs — DOC-48 §5.4 gives this subcommand no `id`
/// argument, but `workstream.deactivate` requires one. Resolves it via one
/// `workstream.list` call reading `active_workstream_id`; if none is active,
/// this is a documented no-op (printed, not an error), matching
/// `crate::workstreams::activation::deactivate`'s own idempotent stance.
pub async fn deactivate(project: &Path) -> Result<()> {
    let exe = tcode_exe::resolve()?;
    let mut client = StdioRpcClient::spawn(&exe, project)?;

    let list_result = client.call("workstream.list", json!({})).await;
    let list_result = match list_result {
        Ok(v) => v,
        Err(e) => {
            client.shutdown(Duration::from_secs(5)).await?;
            return Err(e.into());
        }
    };
    let active_id = list_result
        .get("active_workstream_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    let Some(active_id) = active_id else {
        client.shutdown(Duration::from_secs(5)).await?;
        println!("no workstream is active");
        return Ok(());
    };

    let result = client
        .call("workstream.deactivate", json!({"id": active_id}))
        .await;
    client.shutdown(Duration::from_secs(5)).await?;
    result?;
    println!("deactivated workstream {active_id}");
    Ok(())
}

/// `tcode workstream close <ID> [--project P]`.
///
/// Why/What: irreversible (DOC-48 §4.4); prints a plain confirmation.
pub async fn close(project: &Path, id: &str) -> Result<()> {
    let exe = tcode_exe::resolve()?;
    let mut client = StdioRpcClient::spawn(&exe, project)?;
    let result = client.call("workstream.close", json!({"id": id})).await;
    client.shutdown(Duration::from_secs(5)).await?;
    result?;
    println!("closed workstream {id}");
    Ok(())
}

/// Turn an `RpcClientError` from `workstream.activate` into a clear message,
/// special-casing `-32008 active_conflict` (DOC-48 §5.2/§6.1,
/// `RpcError::active_conflict`'s `{"error_type": "active_conflict",
/// "active_id": ...}` data shape) to name the currently-active workstream
/// and suggest `--force`.
///
/// Why: the raw `RpcClientError::Rpc` `Display` ("daemon returned an error
/// (-32008): another workstream is active: <id>") already contains the
/// active id, but doesn't tell the operator what to DO about it — this is
/// the one place that turns the wire error into an actionable instruction.
/// What: pattern-matches `code == -32008`, pulling `active_id` out of the
/// error's `data` payload; any other error passes through unchanged (`.into()`).
/// Test: `tests::explain_activation_error_names_the_active_workstream_and_suggests_force`,
/// `tests::explain_activation_error_passes_through_other_errors`.
fn explain_activation_error(err: RpcClientError, target_id: &str) -> anyhow::Error {
    if let RpcClientError::Rpc {
        code: -32008,
        ref data,
        ..
    } = err
    {
        let active_id = data
            .as_ref()
            .and_then(|d| d.get("active_id"))
            .and_then(|v| v.as_str())
            .unwrap_or("<unknown>");
        return anyhow::anyhow!(
            "workstream {active_id} is already active; pass --force to switch to {target_id}"
        );
    }
    err.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explain_activation_error_names_the_active_workstream_and_suggests_force() {
        let err = RpcClientError::Rpc {
            code: -32008,
            message: "another workstream is active: a1b2c3d4-e5f6-4748-9a0b-1c2d3e4f5a6b"
                .to_string(),
            data: Some(json!({
                "error_type": "active_conflict",
                "active_id": "a1b2c3d4-e5f6-4748-9a0b-1c2d3e4f5a6b",
            })),
        };
        let message =
            explain_activation_error(err, "b2c3d4e5-f6a1-4748-9a0b-1c2d3e4f5a6b").to_string();
        assert!(
            message.contains("a1b2c3d4-e5f6-4748-9a0b-1c2d3e4f5a6b"),
            "must name the currently-active workstream: {message}"
        );
        assert!(
            message.contains("--force"),
            "must suggest --force: {message}"
        );
        assert!(
            message.contains("b2c3d4e5-f6a1-4748-9a0b-1c2d3e4f5a6b"),
            "must name the target workstream: {message}"
        );
    }

    #[test]
    fn explain_activation_error_passes_through_other_errors() {
        let err = RpcClientError::Rpc {
            code: -32002,
            message: "workstream not found: nonexistent".to_string(),
            data: None,
        };
        let message = explain_activation_error(err, "nonexistent").to_string();
        assert!(
            message.contains("workstream not found"),
            "a non-active_conflict error must pass through unchanged: {message}"
        );
    }
}
