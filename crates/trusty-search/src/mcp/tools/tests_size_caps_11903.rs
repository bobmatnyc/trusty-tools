//! The MCP `create_index` tool carries the #11655 size caps (#11903).
//!
//! Why: before #11903 the tool's schema listed neither cap and its arm built
//! the create body without them, so a cap sent through MCP answered success
//! and the index got the defaults. The guarantee is about what the DAEMON
//! stores and answers, so these cases drive the MCP dispatcher over a scratch
//! socket served by the real `search.index.create` router, not a mock.
//! What: a cap is stored on the registered handle; an out-of-range cap is
//! refused with `invalid_size_cap` and registers nothing; a re-registration
//! with other caps answers `size_caps_applied: false` and the message; the
//! schema lists both fields with descriptions.
//! Test: this module.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};
use trusty_common::uds::server::{serve_until, RpcRouter, RpcServeOptions};

use super::tests::req;
use super::{tool_descriptors, McpServer};
use crate::allowlist::{AllowlistConfig, AllowlistEntry, AllowlistPaths};
use crate::core::embed::{Embedder, MockEmbedder};
use crate::core::registry::{IndexId, IndexRegistry};
use crate::service::daemon_client::DaemonClient;
use crate::service::rpc::writes;
use crate::service::server::tests_components::IsolatedDataDir;
use crate::service::server::SearchAppState;
use crate::service::size_caps::FileSizeCaps;

/// A daemon serving the real write router on a scratch socket.
struct LiveDaemon {
    state: Arc<SearchAppState>,
    server: McpServer,
    _dir: tempfile::TempDir,
    _stop: tokio::sync::oneshot::Sender<()>,
}

/// A real directory under `$HOME`: `$TMPDIR` is on the hard denylist on macOS,
/// which would refuse the root before the size-cap check runs.
fn safe_root(name: &str) -> PathBuf {
    let dir = dirs::home_dir()
        .expect("HOME required")
        .join(".trusty-search-11903-mcp")
        .join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create test root");
    std::fs::canonicalize(&dir).expect("canonicalize test root")
}

/// An allowlist under `dir` approving exactly `root`.
fn allowlist(dir: &Path, root: &Path) -> AllowlistPaths {
    let paths = AllowlistPaths::default()
        .with_allowlist(dir.join("allowlist.toml"))
        .with_project_paths(dir.join("projects.json"));
    AllowlistConfig {
        entries: vec![AllowlistEntry {
            path: root.to_path_buf(),
            name: None,
            exclude: Vec::new(),
            extensions: Vec::new(),
            skip_kg: false,
        }],
    }
    .save_to(&paths.allowlist_file())
    .expect("write allowlist");
    paths
}

/// Serve `search.index.create` over a scratch socket for an empty registry
/// whose allowlist approves `root`, and hand back an MCP server dialling it.
async fn live_daemon(isolated: &IsolatedDataDir, root: &Path) -> LiveDaemon {
    let state = Arc::new(
        SearchAppState::new(IndexRegistry::new())
            .with_allowlist_paths(allowlist(isolated.path(), root)),
    );
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(8));
    state.install_embedder(embedder).await;
    let router = Arc::new(writes::register(RpcRouter::new(), &state));
    let dir = tempfile::tempdir().expect("scratch socket dir");
    let socket = dir.path().join("ts.sock");
    let listener = trusty_common::uds::bind_hardened(&socket).expect("bind the scratch socket");
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        serve_until(&listener, router, RpcServeOptions::default(), async {
            let _ = stopped.await;
        })
        .await;
    });
    LiveDaemon {
        state,
        server: McpServer::new(DaemonClient::at(socket)),
        _dir: dir,
        _stop: stop,
    }
}

/// Dispatch `create_index` and return the daemon's JSON answer.
async fn create_ok(server: &McpServer, args: Value) -> Value {
    let resp = server.dispatch(req("create_index", args)).await;
    assert!(resp.error.is_none(), "unexpected error: {:?}", resp.error);
    let result = resp.result.expect("a result");
    let text = result["content"][0]["text"].as_str().expect("text node");
    serde_json::from_str(text).expect("JSON body")
}

/// #11903 criterion 2: a cap sent through MCP is the cap the index holds, and
/// an omitted cap keeps its default.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn mcp_create_index_stores_the_size_cap_it_was_sent() {
    let isolated = IsolatedDataDir::new();
    let root = safe_root("caps-stored");
    let daemon = live_daemon(&isolated, &root).await;

    let body = create_ok(
        &daemon.server,
        json!({ "id": "caps-stored-11903", "root_path": root, "source_file_max_bytes": 2_097_152u64 }),
    )
    .await;
    assert_eq!(body["created"], true, "{body}");
    let handle = daemon
        .state
        .registry
        .get(&IndexId::new("caps-stored-11903"))
        .expect("registered");
    assert_eq!(handle.size_caps.source, 2_097_152, "the MCP cap is stored");
    assert_eq!(
        handle.size_caps.office,
        FileSizeCaps::default().office,
        "an omitted cap keeps its default"
    );
}

/// #11903 criterion 3: an out-of-range cap through MCP is refused with
/// `invalid_size_cap`, never clamped or defaulted, and registers nothing.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn mcp_create_index_refuses_an_out_of_range_size_cap() {
    let isolated = IsolatedDataDir::new();
    let root = safe_root("caps-bad");
    let daemon = live_daemon(&isolated, &root).await;

    for (field, value) in [
        ("office_file_max_bytes", 0u64),
        ("source_file_max_bytes", 256 * 1024 * 1024 + 1),
    ] {
        let mut args = json!({ "id": "caps-bad-11903", "root_path": root });
        args[field] = json!(value);
        let resp = daemon.server.dispatch(req("create_index", args)).await;
        let error = resp
            .error
            .unwrap_or_else(|| panic!("{field}={value} must be refused: {:?}", resp.result));
        assert!(
            error.message.contains("invalid_size_cap"),
            "{field}={value}: {}",
            error.message
        );
        assert!(
            daemon
                .state
                .registry
                .get(&IndexId::new("caps-bad-11903"))
                .is_none(),
            "{field}={value}: a refused create registers nothing"
        );
    }
}

/// #11903 criterion 4: re-registering through MCP with other caps answers
/// `size_caps_applied: false` and the message, and the held caps stay.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn mcp_create_index_reports_changed_caps_on_re_registration_as_not_applied() {
    let isolated = IsolatedDataDir::new();
    let root = safe_root("caps-rereg");
    let daemon = live_daemon(&isolated, &root).await;
    let first = create_ok(
        &daemon.server,
        json!({ "id": "caps-rereg-11903", "root_path": root }),
    )
    .await;
    assert_eq!(first["created"], true, "{first}");

    let again = create_ok(
        &daemon.server,
        json!({ "id": "caps-rereg-11903", "root_path": root, "office_file_max_bytes": 52_428_800u64 }),
    )
    .await;
    assert_eq!(again["created"], false, "{again}");
    assert_eq!(again["size_caps_applied"], false, "{again}");
    assert_eq!(
        again["requested_size_caps"]["office_file_max_bytes"], 52_428_800u64,
        "{again}"
    );
    let message = again["message"].as_str().unwrap_or("");
    assert!(message.contains("re-index"), "{again}");
    let handle = daemon
        .state
        .registry
        .get(&IndexId::new("caps-rereg-11903"))
        .expect("registered");
    assert_eq!(handle.size_caps.office, FileSizeCaps::default().office);
}

/// #11903 criterion 1: the `create_index` schema lists both caps as optional
/// integers with descriptions.
/// Test: this function IS the test.
#[test]
fn create_index_schema_lists_both_size_caps() {
    let tools = tool_descriptors();
    let create = tools
        .as_array()
        .expect("tools array")
        .iter()
        .find(|t| t["name"] == "create_index")
        .expect("create_index descriptor");
    let schema = &create["inputSchema"];
    for field in ["source_file_max_bytes", "office_file_max_bytes"] {
        let prop = &schema["properties"][field];
        assert_eq!(prop["type"], "integer", "{field}: {prop}");
        let description = prop["description"].as_str().unwrap_or("");
        assert!(description.contains("268435456"), "{field}: {description}");
        assert!(
            !schema["required"]
                .as_array()
                .expect("required")
                .contains(&json!(field)),
            "{field} must stay optional"
        );
    }
}
