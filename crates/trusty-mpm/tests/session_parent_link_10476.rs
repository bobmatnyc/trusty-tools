//! A session spawned through MCP `session_new` records its parent (#10476).
//!
//! Why: `tm tui` draws a bridge-spawned worker under the session that made it.
//! The bridge stamps `parent_session_id`; the daemon validates it, stores it on
//! the new record, and returns it from the list API and the MCP `session_list`.
//! A spawn with no parent, or with a bad one, must look exactly as before.
//! What: each test drives the real MCP dispatch (`trusty_mpm::mcp::dispatch`
//! over `StateBackend`) or `spawn_managed` against a temp git repository with
//! no `origin`, a fake tmux driver, and a scratch `$HOME` whose config enables
//! MCP spawning for that registered checkout.
//! Test: this file IS the test module (`env_serial` target: it rewrites `$HOME`).

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use serde_json::{Value, json};
use serial_test::serial;
use tempfile::TempDir;

use trusty_mpm::daemon::managed_routes::{SpawnParams, list_managed_sessions, spawn_managed};
use trusty_mpm::daemon::mcp_backend::StateBackend;
use trusty_mpm::daemon::state::DaemonState;
use trusty_mpm::mcp::OrchestratorBackend;
use trusty_mpm::project::Project;
use trusty_mpm::session_manager::{ManagedSessionId, SessionRecord};

/// Restores `$HOME` on drop; every caller is `#[serial]` in `env_serial`.
struct HomeGuard {
    prev: Option<std::ffi::OsString>,
    _dir: TempDir,
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        // SAFETY: `env_serial` runs one test at a time, and each is `#[serial]`.
        unsafe {
            match self.prev.take() {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }
}

/// A scratch `$HOME` whose config enables MCP spawning and lifts the disk gate.
fn scratch_home() -> HomeGuard {
    let home = TempDir::new().expect("scratch home");
    let dir = home.path().join(".trusty-tools").join("trusty-mpm");
    std::fs::create_dir_all(&dir).expect("config dir");
    std::fs::write(
        dir.join("config.yaml"),
        "daemon:\n  allow_mcp_spawn: true\ndisk:\n  max_usage_pct: 100\n",
    )
    .expect("config");
    let prior = std::env::var_os("HOME");
    // SAFETY: see `HomeGuard::drop`.
    unsafe { std::env::set_var("HOME", home.path()) };
    HomeGuard {
        prev: prior,
        _dir: home,
    }
}

/// Run git in `dir`, asserting success.
fn git(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .output()
        .expect("git runs");
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

/// One isolated daemon, one registered local-only checkout, one parent session.
struct Fixture {
    _home: HomeGuard,
    data: TempDir,
    checkout: TempDir,
    state: Arc<DaemonState>,
    parent: SessionRecord,
}

async fn fixture() -> Fixture {
    let home = scratch_home();
    let data = TempDir::new().expect("data root");
    let state = Arc::new(DaemonState::with_root_isolated_managed(data.path().to_path_buf()).await);
    let checkout = TempDir::new().expect("checkout");
    git(checkout.path(), &["init", "-q", "-b", "main"]);
    git(
        checkout.path(),
        &["commit", "-q", "--allow-empty", "-m", "init"],
    );
    state
        .project_registry()
        .await
        .register(Project {
            name: "parent-link".into(),
            repo_url: checkout.path().to_string_lossy().into_owned(),
            default_branch: "main".into(),
            stack_hint: None,
            tags: vec![],
            description: None,
            gh_user: None,
            gh_account: None,
            github: None,
            commit_name: None,
            commit_email: None,
            worktree: None,
            project_key: None,
        })
        .await
        .expect("register project");
    let parent = state
        .session_manager()
        .await
        .create(
            "parent".into(),
            Some(data.path().join("p")),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create parent");
    Fixture {
        _home: home,
        data,
        checkout,
        state,
        parent,
    }
}

impl Fixture {
    /// Call the MCP `session_new` tool through the real dispatcher.
    async fn mcp_session_new(&self, parent: Option<Value>) -> Value {
        let mut args = json!({
            "repo_url": self.checkout.path().to_string_lossy(),
            "ref": "main",
            "task": "worker",
            "ephemeral": true,
        });
        if let (Some(p), Some(obj)) = (parent, args.as_object_mut()) {
            obj.insert("parent_session_id".into(), p);
        }
        let req = trusty_mcp::Request {
            jsonrpc: Some("2.0".into()),
            id: Some(json!(1)),
            method: "tools/call".into(),
            params: Some(json!({ "name": "session_new", "arguments": args })),
        };
        let resp = trusty_mpm::mcp::dispatch(&StateBackend::new(self.state.clone()), req).await;
        let result = resp.result.expect("tools/call result");
        let text = result["content"][0]["text"].as_str().expect("text");
        assert_eq!(result["isError"], false, "session_new failed: {text}");
        serde_json::from_str(text).expect("session_new returns the record as JSON")
    }

    fn sessions_json(&self) -> Value {
        let raw =
            std::fs::read_to_string(find_sessions_json(self.data.path())).expect("sessions.json");
        serde_json::from_str(&raw).expect("sessions.json parses")
    }

    /// `GET /api/v1/sessions/managed`, decoded.
    async fn list_api(&self) -> Vec<Value> {
        use axum::response::IntoResponse;
        let resp = list_managed_sessions(
            axum::extract::State(self.state.clone()),
            axum::extract::Query(HashMap::new()),
        )
        .await
        .into_response();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        let body: Value = serde_json::from_slice(&bytes).expect("list JSON");
        body["sessions"].as_array().expect("sessions").clone()
    }

    async fn mcp_session_list(&self) -> Vec<Value> {
        let list = StateBackend::new(self.state.clone())
            .session_list()
            .await
            .expect("session_list");
        list.as_array().expect("array").clone()
    }
}

/// Locate `sessions.json` under the isolated data root, wherever it nests.
fn find_sessions_json(root: &Path) -> std::path::PathBuf {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.file_name().is_some_and(|n| n == "sessions.json") {
                return p;
            }
        }
    }
    panic!("no sessions.json under {}", root.display());
}

fn row<'a>(rows: &'a [Value], id: &str) -> &'a Value {
    rows.iter()
        .find(|r| r["id"] == id)
        .unwrap_or_else(|| panic!("{id} missing from {rows:?}"))
}

/// Criterion 1: a bridge-stamped parent is stored, and both read surfaces
/// return it.
#[tokio::test]
#[serial]
async fn session_new_records_valid_parent() {
    let fx = fixture().await;
    let parent_id = fx.parent.id.to_string();

    let child = fx.mcp_session_new(Some(json!(parent_id))).await;
    let child_id = child["id"].as_str().expect("child id").to_string();
    assert_eq!(child["parent_session_id"], parent_id.as_str());

    let store = fx.sessions_json();
    assert_eq!(
        store["sessions"][&child_id]["parent_session_id"],
        parent_id.as_str()
    );
    assert!(
        store["sessions"][&parent_id]
            .get("parent_session_id")
            .is_none()
    );

    let listed = fx.list_api().await;
    assert_eq!(
        row(&listed, &child_id)["parent_session_id"],
        parent_id.as_str()
    );
    assert!(row(&listed, &parent_id).get("parent_session_id").is_none());

    let mcp = fx.mcp_session_list().await;
    assert_eq!(
        row(&mcp, &child_id)["parent_session_id"],
        parent_id.as_str()
    );
}

/// Criterion 2: a spawn with no bridge caller — the `tm sessions new` path,
/// which reaches `spawn_managed` with no parent — stores no key at all.
#[tokio::test]
#[serial]
async fn cli_spawn_has_no_parent_key() {
    let fx = fixture().await;
    let params = SpawnParams {
        repo_url: fx.checkout.path().to_string_lossy().into_owned(),
        git_ref: "main".into(),
        task: "cli".into(),
        name_hint: None,
        runtime: None,
        ephemeral: Some(true),
        mcp_initiated: false,
        inject_task: None,
        deliverable_id: None,
        force_new: true,
        worktree: false,
    };
    let record = spawn_managed(&fx.state, ManagedSessionId::new(), params)
        .await
        .expect("CLI-origin spawn");
    assert_eq!(record.parent_session_id, None);
    let store = fx.sessions_json();
    let raw = serde_json::to_string(&store["sessions"][record.id.to_string()]).expect("row");
    assert!(!raw.contains("parent_session_id"), "{raw}");
    let listed = fx.list_api().await;
    assert!(
        row(&listed, &record.id.to_string())
            .get("parent_session_id")
            .is_none()
    );
}

/// Criterion 3, failure path: an id naming no stored record — or a deleted
/// one — is dropped, and the spawn still succeeds.
#[tokio::test]
#[serial]
async fn session_new_unknown_parent_is_none_and_spawn_ok() {
    let fx = fixture().await;
    let child = fx
        .mcp_session_new(Some(json!(ManagedSessionId::new().to_string())))
        .await;
    assert!(child.get("parent_session_id").is_none(), "{child}");

    let mgr = fx.state.session_manager().await;
    mgr.delete_record(&fx.parent.id, true)
        .await
        .expect("delete parent");
    drop(mgr);
    let child = fx
        .mcp_session_new(Some(json!(fx.parent.id.to_string())))
        .await;
    assert!(child.get("parent_session_id").is_none(), "{child}");
    let store = fx.sessions_json();
    assert!(
        !serde_json::to_string(&store)
            .expect("store")
            .contains("parent_session_id"),
        "{store}"
    );
}

/// Criterion 3, failure path: a malformed id (not a UUID, wrong JSON type, or
/// blank) is dropped, and the spawn still succeeds.
#[tokio::test]
#[serial]
async fn session_new_malformed_parent_dropped() {
    let fx = fixture().await;
    for bad in [json!("not-a-uuid"), json!(""), json!(42)] {
        let child = fx.mcp_session_new(Some(bad.clone())).await;
        assert!(child.get("parent_session_id").is_none(), "{bad}: {child}");
    }
    let store = fx.sessions_json();
    assert!(
        !serde_json::to_string(&store)
            .expect("store")
            .contains("parent_session_id"),
        "{store}"
    );
}
