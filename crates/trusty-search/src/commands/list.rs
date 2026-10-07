//! Handler for `trusty-search list`.

use anyhow::{bail, Result};
use colored::Colorize;
use trusty_search::service::daemon_client::DaemonClient;
use trusty_search::service::rpc::reads::METHOD_INDEXES_LIST;

/// Why: extracted so `main()` doesn't inline the index-list plumbing.
/// What: fetches the index list over the daemon socket (#9214,
/// `search.indexes.list`, the twin of `GET /indexes`), prints it as plain text
/// or JSON depending on the global `--json` flag. Returns `Err` when the daemon
/// is unreachable; `main()` prints the friendly red-✗ line and exits 1 (issue
/// #104).
/// Test: `list_reads_the_index_list_over_the_socket`.
pub async fn handle_list(json: bool) -> Result<()> {
    // #9214: the socket, never the retiring HTTP listener.
    let client = DaemonClient::resolve()?;
    crate::commands::daemon_guard::ensure_daemon_up(&client).await?;
    let body = fetch_index_list(&client).await?;
    if json {
        println!("{}", body);
    } else {
        println!("{}", "Registered indexes:".bold());
        let empty: Vec<serde_json::Value> = Vec::new();
        let arr = body
            .get("indexes")
            .and_then(|v| v.as_array())
            .unwrap_or(&empty);
        if arr.is_empty() {
            println!("  {}", "(none)".dimmed());
        } else {
            for v in arr {
                if let Some(s) = v.as_str() {
                    println!("  • {}", s);
                }
            }
        }
        print_parked(&body);
    }
    Ok(())
}

/// The daemon's index list — the body `GET /indexes` answered.
///
/// # Errors
///
/// When the socket is unreachable, or the daemon refuses the call.
pub(crate) async fn fetch_index_list(client: &DaemonClient) -> Result<serde_json::Value> {
    match client
        .call(METHOD_INDEXES_LIST, serde_json::json!({}))
        .await
    {
        Ok(body) => Ok(body),
        Err(e) if e.is_unreachable() => bail!("could not reach daemon: {e}"),
        Err(e) => bail!("daemon returned {e}"),
    }
}

/// Print the `parked` rows of a `GET /indexes` body (#8727).
///
/// Why: a parked registration still owns its root for the create-time overlap
/// check, so hiding it left a `409` pointing at an index `list` never showed.
/// What: one line per row — id, root, and a marker when the root is gone.
/// Nothing is printed when the daemon sent no parked rows.
pub(crate) fn print_parked(body: &serde_json::Value) {
    let Some(rows) = body.get("parked").and_then(|v| v.as_array()) else {
        return;
    };
    if rows.is_empty() {
        return;
    }
    println!(
        "{}",
        "Parked (registered, not resident; still owns its root):".bold()
    );
    for row in rows {
        let id = row.get("id").and_then(|v| v.as_str()).unwrap_or("?");
        let root = row.get("root_path").and_then(|v| v.as_str()).unwrap_or("");
        let gone = row.get("root_state").and_then(|v| v.as_str()) == Some("orphaned");
        let marker = if gone { "  (root missing)" } else { "" };
        println!("  • {id}  {}{}", root.dimmed(), marker.yellow());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::mock_socket::mock_daemon;

    /// #9214: `list` reads `search.indexes.list` with empty params — the
    /// `GET /indexes` default — and returns the daemon's body unchanged.
    #[tokio::test]
    async fn list_reads_the_index_list_over_the_socket() {
        let daemon = mock_daemon(|method, params| {
            assert_eq!(method, METHOD_INDEXES_LIST);
            assert_eq!(params, serde_json::json!({}));
            Ok(serde_json::json!({"indexes": ["a", "b"]}))
        })
        .await;
        let body = fetch_index_list(&daemon.client).await.expect("listed");
        assert_eq!(body["indexes"], serde_json::json!(["a", "b"]));
    }

    /// #9214: an absent socket fails closed, naming it, with no URL.
    #[tokio::test]
    async fn list_fails_closed_when_the_socket_is_absent() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let socket = dir.path().join("absent.sock");
        let err = fetch_index_list(&DaemonClient::at(&socket))
            .await
            .expect_err("nothing serves the socket");
        let text = err.to_string();
        assert!(text.starts_with("could not reach daemon"), "{text}");
        assert!(text.contains(&socket.display().to_string()), "{text}");
        assert!(!text.contains("http://"), "{text}");
    }
}
