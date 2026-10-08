//! Handler for `trusty-search status` (and the `health` alias).

use super::format::format_with_commas;
use anyhow::Result;
use colored::Colorize;
use trusty_search::service::daemon_client::DaemonClient;
use trusty_search::service::rpc::reads::METHOD_INDEX_STATUS;

/// Why: ensures the daemon is up (auto-starts if not), then reads its health,
/// index list, and per-index status and renders or emits JSON. Both
/// `status` and `health` share this entry point so the table only lives in
/// one place.
/// What: #9214 — every read goes over the daemon socket (`search.health`,
/// `search.indexes.list`, `search.index.status`), never HTTP.
/// Test: `status_reads_health_list_and_each_index_over_the_socket`,
/// `status_reports_not_running_when_the_socket_is_absent`,
/// `daemon_location_prefers_the_http_address_the_daemon_reports`.
pub async fn handle_status(json: bool) -> Result<()> {
    let client = DaemonClient::resolve()?;
    crate::commands::daemon_guard::ensure_daemon_up(&client).await?;
    let Some(report) = gather_status(&client).await else {
        if json {
            println!(r#"{{"daemon":"not_running"}}"#);
            // JSON consumers parse the body; suppress the central ✗ line.
            return Err(anyhow::anyhow!(""));
        }
        anyhow::bail!("Daemon not running  (start with `trusty-search start`)");
    };
    render_status(&report, json)
}

/// Everything `status` prints, read from the daemon.
struct StatusReport {
    /// Where the daemon answers — its HTTP URL when it binds one.
    location: String,
    health: serde_json::Value,
    list: serde_json::Value,
    /// `(index id, status body)`, sorted by id.
    per_index: Vec<(String, serde_json::Value)>,
}

/// The daemon's address as `status` has always printed it.
///
/// Why (#9214): the URL used to be the HTTP base the CLI dialled. Over the
/// socket, `search.health`'s `transport.http_addr` names the same listener, so
/// the line is unchanged for an HTTP-bound daemon; a `--no-http` daemon is
/// named by its socket.
fn daemon_location(health: &serde_json::Value, socket: &std::path::Path) -> String {
    match health
        .pointer("/transport/http_addr")
        .and_then(|v| v.as_str())
    {
        Some(addr) => format!("http://{addr}"),
        None => socket.display().to_string(),
    }
}

/// Read the health, the index list, and every index's status.
///
/// What: `None` when `search.health` does not answer; a list or per-index
/// failure degrades to an empty body, as it did over HTTP.
async fn gather_status(client: &DaemonClient) -> Option<StatusReport> {
    let health = client.health().await.ok()?;
    let list_body = super::list::fetch_index_list(client)
        .await
        .unwrap_or_else(|_| serde_json::json!({"indexes": []}));
    let empty: Vec<serde_json::Value> = Vec::new();
    let names: Vec<String> = list_body
        .get("indexes")
        .and_then(|v| v.as_array())
        .unwrap_or(&empty)
        .iter()
        .filter_map(|v| v.as_str().map(|s| s.to_string()))
        .collect();

    // Fetch per-index status concurrently.
    let mut joinset = tokio::task::JoinSet::new();
    for name in &names {
        let n = name.clone();
        let c = client.clone();
        joinset.spawn(async move {
            let body = c
                .call(METHOD_INDEX_STATUS, serde_json::json!({ "index_id": n }))
                .await
                .unwrap_or_else(|_| serde_json::json!({}));
            (n, body)
        });
    }
    let mut per_index: Vec<(String, serde_json::Value)> = Vec::new();
    while let Some(j) = joinset.join_next().await {
        if let Ok(pair) = j {
            per_index.push(pair);
        }
    }
    per_index.sort_by(|a, b| a.0.cmp(&b.0));
    Some(StatusReport {
        location: daemon_location(&health, client.socket()),
        health,
        list: list_body,
        per_index,
    })
}

/// Print `report` as the table or the `--json` document.
fn render_status(report: &StatusReport, json: bool) -> Result<()> {
    let StatusReport {
        location: base,
        health: health_body,
        list: list_body,
        per_index,
    } = report;
    if json {
        let arr: Vec<serde_json::Value> = per_index
            .iter()
            .map(|(n, b)| serde_json::json!({"id": n, "status": b}))
            .collect();
        println!(
            "{}",
            serde_json::json!({
                "daemon": "running",
                "url": base,
                "version": health_body.get("version").cloned().unwrap_or(serde_json::json!(null)),
                "indexes": arr,
                // #8727: the registrations `indexes` omits but create still checks.
                "parked": list_body.get("parked").cloned().unwrap_or(serde_json::json!([])),
            })
        );
    } else {
        let version = health_body
            .get("version")
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        println!(
            "{} Daemon running  {}  v{}",
            "✓".green(),
            base.cyan(),
            version
        );
        if per_index.is_empty() {
            println!("{}", "Indexes:".bold());
            println!("  {}", "(none)".dimmed());
        } else {
            println!("{}", "Indexes:".bold());
            for (name, body) in per_index.iter() {
                let chunks = body
                    .get("chunk_count")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let root = body.get("root_path").and_then(|v| v.as_str()).unwrap_or("");
                let chunks_fmt = format_with_commas(chunks);
                if root.is_empty() {
                    println!("  {:<16} {:>12} chunks", name.bold(), chunks_fmt,);
                } else {
                    println!(
                        "  {:<16} {:>12} chunks  {}",
                        name.bold(),
                        chunks_fmt,
                        root.dimmed()
                    );
                }
            }
        }
        super::list::print_parked(list_body);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::mock_socket::mock_daemon;
    use trusty_search::service::rpc::reads::METHOD_INDEXES_LIST;
    use trusty_search::service::socket::METHOD_HEALTH;

    /// #9214: `status` reads all three methods over the socket and sorts the
    /// per-index rows, each fetched with its own `index_id`.
    #[tokio::test]
    async fn status_reads_health_list_and_each_index_over_the_socket() {
        let daemon = mock_daemon(|method, params| match method {
            METHOD_HEALTH => Ok(serde_json::json!({
                "version": "9.9.9",
                "transport": {"http_addr": "127.0.0.1:7999", "socket_path": "/s"},
            })),
            METHOD_INDEXES_LIST => Ok(serde_json::json!({"indexes": ["b", "a"]})),
            METHOD_INDEX_STATUS => Ok(serde_json::json!({
                "chunk_count": if params["index_id"] == "a" { 1 } else { 2 },
            })),
            other => panic!("unexpected method {other}"),
        })
        .await;
        let report = gather_status(&daemon.client).await.expect("daemon answers");
        assert_eq!(report.location, "http://127.0.0.1:7999");
        assert_eq!(report.health["version"], "9.9.9");
        let rows: Vec<(&str, u64)> = report
            .per_index
            .iter()
            .map(|(n, b)| (n.as_str(), b["chunk_count"].as_u64().unwrap()))
            .collect();
        assert_eq!(rows, vec![("a", 1), ("b", 2)]);
    }

    /// #9214: an absent socket is "not running", never a dial of TCP.
    #[tokio::test]
    async fn status_reports_not_running_when_the_socket_is_absent() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let client = DaemonClient::at(dir.path().join("absent.sock"));
        assert!(gather_status(&client).await.is_none());
    }

    /// #9214: the printed location is the daemon's HTTP URL when it binds one,
    /// else its socket.
    #[test]
    fn daemon_location_prefers_the_http_address_the_daemon_reports() {
        let socket = std::path::Path::new("/tmp/ts.sock");
        let bound = serde_json::json!({"transport": {"http_addr": "127.0.0.1:7001"}});
        assert_eq!(daemon_location(&bound, socket), "http://127.0.0.1:7001");
        let socket_only = serde_json::json!({"transport": {"http_addr": null}});
        assert_eq!(daemon_location(&socket_only, socket), "/tmp/ts.sock");
    }
}
