//! Handler for `trusty-search list`.

use super::daemon_utils::daemon_base_url;
use anyhow::{bail, Result};
use colored::Colorize;

/// Why: extracted so `main()` doesn't inline the GET `/indexes` plumbing.
/// What: fetches the index list, prints it as plain text or JSON depending on
/// the global `--json` flag. Returns `Err` when the daemon is unreachable;
/// `main()` prints the friendly red-✗ line and exits 1 (issue #104).
/// Test: `cargo run -- list` against a running daemon prints registered ids.
pub async fn handle_list(json: bool) -> Result<()> {
    let base = daemon_base_url();
    crate::commands::daemon_guard::ensure_daemon_running_or_exit(&base).await?;
    let url = format!("{}/indexes", base);
    let list_client = trusty_common::server::daemon_http_client()?;
    match list_client.get(&url).send().await {
        Ok(resp) if resp.status().is_success() => {
            let body: serde_json::Value =
                resp.json().await.unwrap_or_else(|_| serde_json::json!({}));
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
        }
        Ok(resp) => bail!("daemon returned {}", resp.status()),
        Err(e) => bail!("could not reach daemon at {}: {e}", base),
    }
    Ok(())
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
