//! Handler for `trusty-search status` (and the `health` alias).

use super::daemon_utils::daemon_base_url;
use super::format::format_with_commas;
use anyhow::Result;
use colored::Colorize;

/// Why: ensures the daemon is up (auto-starts if not), then queries `/health`,
/// `/indexes`, and per-index `/status` and renders or emits JSON. Both
/// `status` and `health` share this entry point so the table only lives in
/// one place.
/// Test: `cargo run -- status` against a running daemon prints the table; with
/// no daemon, it auto-starts and then prints the table.
pub async fn handle_status(json: bool) -> Result<()> {
    crate::commands::daemon_guard::ensure_daemon_running_or_exit(&daemon_base_url()).await?;
    run_status(json).await
}

/// Shared handler for `status` and `health` — both show the same rich output.
async fn run_status(json: bool) -> Result<()> {
    let base = daemon_base_url();
    let client = trusty_common::server::daemon_http_client()?;

    let health = client.get(format!("{}/health", base)).send().await;
    let health_body: serde_json::Value = match health {
        Ok(r) if r.status().is_success() => {
            r.json().await.unwrap_or_else(|_| serde_json::json!({}))
        }
        _ => {
            if json {
                println!(r#"{{"daemon":"not_running"}}"#);
                // JSON consumers parse the body; suppress the central ✗ line.
                return Err(anyhow::anyhow!(""));
            }
            anyhow::bail!("Daemon not running  (start with `trusty-search start`)");
        }
    };

    let list = client.get(format!("{}/indexes", base)).send().await;
    let list_body: serde_json::Value = match list {
        Ok(r) if r.status().is_success() => {
            r.json().await.unwrap_or_else(|_| serde_json::json!({}))
        }
        _ => serde_json::json!({"indexes": []}),
    };
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
        let url = format!("{}/indexes/{}/status", base, n);
        let c = client.clone();
        joinset.spawn(async move {
            let body: serde_json::Value = match c.get(&url).send().await {
                Ok(r) if r.status().is_success() => {
                    r.json().await.unwrap_or_else(|_| serde_json::json!({}))
                }
                _ => serde_json::json!({}),
            };
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
            for (name, body) in &per_index {
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
        super::list::print_parked(&list_body);
    }
    Ok(())
}
