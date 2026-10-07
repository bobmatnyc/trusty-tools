//! Handler for `trusty-search remove <file>`.

use super::index_resolve::{print_index_header, resolve_index};
use anyhow::{bail, Result};
use colored::Colorize;
use trusty_search::service::daemon_client::DaemonClient;
use trusty_search::service::rpc::writes::METHOD_INDEX_FILE_REMOVE;

/// Why: extracted so `main()` doesn't have to inline the daemon plumbing.
/// What: #9214 — sends `search.index.file.remove` (the twin of
/// `POST /indexes/<id>/remove-file`) over the daemon socket; returns `Err`
/// when the daemon is unreachable or refuses, so `main()` can print the
/// friendly ✗ line and exit (issue #104).
/// Test: `remove_sends_the_file_over_the_socket`,
/// `remove_reports_the_daemons_refusal`.
pub async fn handle_remove(
    explicit_index: &Option<String>,
    file: std::path::PathBuf,
) -> Result<()> {
    let (index_id, warned) = resolve_index(explicit_index)?;
    print_index_header(&index_id, warned);
    let client = DaemonClient::resolve()?;
    crate::commands::daemon_guard::ensure_daemon_up(&client).await?;
    remove_file(&client, &index_id, &file).await?;
    println!("{} [{}] removed {}", "−".red(), index_id, file.display());
    Ok(())
}

/// Drop `file`'s chunks from `index_id`.
///
/// # Errors
///
/// When the socket is unreachable, or the daemon refuses the removal.
async fn remove_file(client: &DaemonClient, index_id: &str, file: &std::path::Path) -> Result<()> {
    let params = serde_json::json!({
        "index_id": index_id,
        "body": { "path": file.display().to_string() },
    });
    match client.call(METHOD_INDEX_FILE_REMOVE, params).await {
        Ok(_) => Ok(()),
        Err(e) if e.is_unreachable() => bail!("could not reach daemon: {e}"),
        Err(e) => bail!("daemon returned {e} for index {index_id}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::mock_socket::mock_daemon;
    use trusty_common::uds::server::RpcError;

    /// #9214: the removal is one `search.index.file.remove` call carrying the
    /// index id and the path in the HTTP body's shape.
    #[tokio::test]
    async fn remove_sends_the_file_over_the_socket() {
        let daemon = mock_daemon(|method, params| {
            assert_eq!(method, METHOD_INDEX_FILE_REMOVE);
            assert_eq!(
                params,
                serde_json::json!({"index_id": "idx", "body": {"path": "src/old.rs"}})
            );
            Ok(serde_json::json!({"removed_chunks": 3}))
        })
        .await;
        remove_file(&daemon.client, "idx", std::path::Path::new("src/old.rs"))
            .await
            .expect("removed");
    }

    /// #9214: a refusal is an error carrying the daemon's own message.
    #[tokio::test]
    async fn remove_reports_the_daemons_refusal() {
        let daemon = mock_daemon(|_, _| {
            Err(RpcError::new(
                trusty_search::service::rpc::error::CODE_NOT_FOUND,
                "unknown index: idx",
            ))
        })
        .await;
        let err = remove_file(&daemon.client, "idx", std::path::Path::new("a.rs"))
            .await
            .expect_err("refused");
        let text = err.to_string();
        assert!(text.starts_with("daemon returned"), "{text}");
        assert!(text.contains("unknown index: idx"), "{text}");
    }
}
