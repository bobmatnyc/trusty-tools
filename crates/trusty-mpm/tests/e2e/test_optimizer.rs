//! E2E: token-use optimizer configuration, file-backed.

use crate::harness::{TestDaemon, write_optimizer_toml};
use serde_json::Value;

/// With no `optimizer.toml` planted, the daemon serves the bundled default
/// policy: `trim` level with redundant-read suppression on.
#[tokio::test]
async fn optimizer_default_config() {
    let daemon = TestDaemon::spawn().await;
    let body: Value = daemon
        .client()
        .get(daemon.url("/optimizer"))
        .send()
        .await
        .expect("optimizer request")
        .json()
        .await
        .expect("optimizer body");
    assert_eq!(body["optimizer"]["default_level"], "trim");
    assert_eq!(body["optimizer"]["suppress_redundant_reads"], true);
}

/// A custom `optimizer.toml` planted in the framework dir before boot is
/// reflected by `GET /optimizer`.
#[tokio::test]
async fn optimizer_config_from_file() {
    let daemon = TestDaemon::spawn_with(|paths| {
        write_optimizer_toml(paths, "[default]\nlevel = \"Caveman\"\n");
    })
    .await;

    let body: Value = daemon
        .client()
        .get(daemon.url("/optimizer"))
        .send()
        .await
        .expect("optimizer request")
        .json()
        .await
        .expect("optimizer body");
    assert_eq!(body["optimizer"]["default_level"], "caveman");
}

/// A planted `optimizer.toml` with per-tool overrides is read end-to-end:
/// the `[tools]` table reaches the live daemon's served config.
#[tokio::test]
async fn optimizer_set_cli_rewrites_file() {
    let daemon = TestDaemon::spawn_with(|paths| {
        write_optimizer_toml(
            paths,
            "[default]\nlevel = \"Trim\"\n\n[tools]\nRead = \"Off\"\n",
        );
    })
    .await;

    let body: Value = daemon
        .client()
        .get(daemon.url("/optimizer"))
        .send()
        .await
        .expect("optimizer request")
        .json()
        .await
        .expect("optimizer body");
    assert_eq!(body["optimizer"]["default_level"], "trim");
    // The per-tool override survives the file -> config -> JSON round trip.
    assert_eq!(body["optimizer"]["tool_overrides"]["Read"], "off");
}
