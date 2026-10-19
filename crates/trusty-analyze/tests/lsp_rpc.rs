//! The `analyze.lsp_*` RPC methods against the scripted fake server (#9078, L3).
//!
//! Why: DOC-72 §5 puts type-checked diagnostics and references on the analyze
//! socket. Their contract is the wire shape, the version stamps, `Stale`, and a
//! typed refusal for every failure; only a real process boundary shows a crash,
//! a hang or a "content modified" storm, and CI has no `rust-analyzer`.
//! What: each test builds daemon state whose LSP manager runs the fake server
//! (`support/fake_lsp_server.rs`) in some mode over a throwaway git work tree,
//! then calls the methods through the real router. Two goldens pin each
//! response shape; `UPDATE_GOLDEN=1` rewrites them.
//! Test: this file is the test.
#![cfg(unix)]

#[path = "support/fake_lsp_server.rs"]
mod fake_lsp_server;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use trusty_analyze::core::{FactStore, ScipOverlayStore, TrustySearchClient};
use trusty_analyze::lsp::{LspConfig, LspManager};
use trusty_analyze::service::events::AnalyzerAppState;
use trusty_analyze::service::rpc::{build_router, METHODS};
use trusty_common::uds::server::{RpcResponse, CODE_INVALID_PARAMS};

/// The JSON-RPC code every typed LSP refusal carries (`data.kind` names it).
const CODE_LSP: i64 = -32006;

/// The entry point the fake server runs under; a no-op in a normal test run.
#[test]
fn fake_lsp_server_entry() {
    fake_lsp_server::run_if_requested();
}

const SOURCE: &str = "pub fn answer() -> u32 {\n    42\n}\n";

/// A git work tree with one committed Rust file, plus daemon state over it.
struct Rig {
    dir: tempfile::TempDir,
    manager: Arc<LspManager>,
    state: AnalyzerAppState,
}

fn git(root: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .current_dir(root)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .expect("run git");
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

impl Rig {
    fn new(mode: &str) -> Self {
        Self::with(mode, |_| {})
    }

    fn with(mode: &str, tweak: impl FnOnce(&mut LspConfig)) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("ws");
        std::fs::create_dir_all(root.join("src")).expect("ws");
        std::fs::write(root.join("src/lib.rs"), SOURCE).expect("lib.rs");
        git(&root, &["init", "-q"]);
        git(&root, &["add", "."]);
        git(&root, &["commit", "-q", "-m", "seed"]);
        let mut config = LspConfig {
            enabled: true,
            rust_analyzer: fake_lsp_server::command(mode, &dir.path().join("g.pid")),
            idle_timeout: Duration::from_secs(600),
            request_timeout: Duration::from_secs(10),
            initialize_timeout: Duration::from_secs(10),
            shutdown_grace: Duration::from_millis(500),
            ..LspConfig::default()
        };
        tweak(&mut config);
        let manager = Arc::new(LspManager::new(config));
        let data = dir.path().join("data");
        std::fs::create_dir_all(&data).expect("data");
        let state = AnalyzerAppState::new(
            TrustySearchClient::new(data.join("absent-search.sock")),
            FactStore::open(&data.join("facts.redb")).expect("facts"),
            ScipOverlayStore::open(&data.join("scip.redb")).expect("scip"),
        )
        .with_lsp(Arc::clone(&manager));
        Self {
            dir,
            manager,
            state,
        }
    }

    fn root(&self) -> PathBuf {
        self.dir.path().join("ws").canonicalize().expect("root")
    }

    fn file(&self) -> PathBuf {
        self.root().join("src/lib.rs")
    }

    async fn call(&self, method: &str, params: Value) -> RpcResponse {
        let frame = serde_json::to_vec(&json!({
            "jsonrpc": "2.0", "id": 1, "method": method, "params": params,
        }))
        .expect("frame");
        build_router(self.state.clone()).dispatch(&frame).await
    }

    async fn ok(&self, method: &str, params: Value) -> Value {
        let r = self.call(method, params).await;
        assert!(r.error.is_none(), "{method} failed: {:?}", r.error);
        r.result.expect("result")
    }

    /// The `data.kind` of a typed LSP refusal, asserting the code.
    async fn refusal(&self, method: &str, params: Value) -> Value {
        let r = self.call(method, params).await;
        let err = r
            .error
            .unwrap_or_else(|| panic!("{method} succeeded: {:?}", r.result));
        assert_eq!(err.code, CODE_LSP, "{method}: {err:?}");
        err.data.expect("typed refusal carries data")
    }

    fn diag_params(&self) -> Value {
        json!({"scope": "file", "path": self.file()})
    }

    fn refs_params(&self) -> Value {
        json!({"path": self.file(), "line": 0, "column": 7})
    }
}

fn assert_snapshot(v: &Value) {
    let s = v["snapshot"].as_str().expect("snapshot is a string");
    let (head, dirty) = s.split_once('+').expect("<head>+<dirty>");
    assert_eq!(head.len(), 40, "{s}");
    assert!(head.chars().all(|c| c.is_ascii_hexdigit()), "{s}");
    assert_eq!(dirty.len(), 16, "{s}");
    assert!(dirty.chars().all(|c| c.is_ascii_hexdigit()), "{s}");
}

// ---- registration ----------------------------------------------------------

#[test]
fn lsp_methods_are_documented() {
    for m in [
        "analyze.lsp_diagnostics",
        "analyze.lsp_references",
        "analyze.lsp_analysed_at",
    ] {
        assert!(METHODS.contains(&m), "{m} missing from METHODS");
    }
}

// ---- diagnostics -----------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn lsp_diagnostics_for_a_file_are_versioned_and_stamped() {
    let rig = Rig::new("normal");
    let v = rig.ok("analyze.lsp_diagnostics", rig.diag_params()).await;
    assert_eq!(v["document_version"], 1);
    assert_eq!(v["complete"], true, "file scope covers the whole file");
    assert_snapshot(&v);
    let file = &v["files"][0];
    assert_eq!(file["path"], json!(rig.file()));
    assert_eq!(file["version"], 1);
    assert_eq!(file["unversioned"], false);
    assert_eq!(file["diagnostics"][0]["message"], "canned diagnostic");
    assert_eq!(
        file["diagnostics"][0]["start"],
        json!({"line": 0, "column": 0})
    );

    // The caller's own version is accepted while it is current.
    let again = rig
        .ok(
            "analyze.lsp_diagnostics",
            json!({"scope": "file", "path": rig.file(), "version": 1}),
        )
        .await;
    assert_eq!(again["document_version"], 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn unversioned_diagnostics_are_accepted_and_never_stale() {
    let rig = Rig::new("unversioned");
    let v = rig
        .ok(
            "analyze.lsp_diagnostics",
            json!({"scope": "file", "path": rig.file(), "version": 1}),
        )
        .await;
    assert_eq!(v["document_version"], 1);
    let file = &v["files"][0];
    assert_eq!(file["version"], Value::Null);
    assert_eq!(file["unversioned"], true);
    assert_eq!(file["diagnostics"][0]["message"], "canned diagnostic");
}

#[tokio::test(flavor = "multi_thread")]
async fn workspace_scope_returns_published_files_marked_incomplete() {
    let rig = Rig::new("normal");
    let v = rig
        .ok(
            "analyze.lsp_diagnostics",
            json!({"scope": "workspace", "path": rig.file()}),
        )
        .await;
    assert_eq!(v["scope"], "workspace");
    // Only what the server has published so far; never a full pass.
    assert_eq!(v["complete"], false, "{v}");
    assert_eq!(v["files"].as_array().map(Vec::len), Some(1));
    assert_eq!(v["files"][0]["path"], json!(rig.file()));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_directory_workspace_scope_is_refused() {
    let rig = Rig::new("normal");
    let r = rig
        .call(
            "analyze.lsp_diagnostics",
            json!({"scope": "workspace", "path": rig.root()}),
        )
        .await;
    let err = r
        .error
        .unwrap_or_else(|| panic!("a directory pass succeeded: {:?}", r.result));
    assert_eq!(err.code, CODE_INVALID_PARAMS, "{err:?}");
    assert!(
        err.message
            .contains("workspace pass not supported in 1.9.0"),
        "{err:?}"
    );
    assert!(rig.manager.live_pids().await.is_empty(), "nothing spawned");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unversioned_publication_from_before_the_change_is_not_fresh() {
    let rig = Rig::with("unversioned_open_only", |c| {
        c.request_timeout = Duration::from_millis(500);
    });
    let v = rig.ok("analyze.lsp_diagnostics", rig.diag_params()).await;
    assert_eq!(v["files"][0]["unversioned"], true);
    // didChange publishes nothing; the didOpen set describes older text.
    std::fs::write(rig.file(), "pub fn answer() -> u32 { 43 }\n").unwrap();
    let data = rig
        .refusal("analyze.lsp_diagnostics", rig.diag_params())
        .await;
    assert_eq!(data["kind"], "lsp_timeout", "{data}");
}

// ---- readiness -------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn references_wait_for_the_server_to_finish_loading() {
    let rig = Rig::new("loading");
    let v = rig.ok("analyze.lsp_references", rig.refs_params()).await;
    // While loading the fake answers `[]`; that must never be the answer.
    assert_eq!(
        v["references"].as_array().map(Vec::len),
        Some(1),
        "an empty list from a loading server became a success: {v}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn diagnostics_wait_for_the_server_to_finish_loading() {
    let rig = Rig::new("loading");
    let v = rig.ok("analyze.lsp_diagnostics", rig.diag_params()).await;
    assert_eq!(
        v["files"][0]["diagnostics"][0]["message"], "canned diagnostic",
        "the loading-time empty set became a success: {v}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_that_never_finishes_loading_is_not_ready() {
    let rig = Rig::with("never_ready", |c| {
        c.request_timeout = Duration::from_millis(500);
    });
    for (method, params) in [
        ("analyze.lsp_references", rig.refs_params()),
        ("analyze.lsp_diagnostics", rig.diag_params()),
    ] {
        let data = rig.refusal(method, params).await;
        assert_eq!(data["kind"], "lsp_not_ready", "{method}: {data}");
    }
    // Not ready is not hung: the server stays up.
    assert_eq!(rig.manager.live_pids().await.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_version_other_than_the_current_one_is_stale() {
    let rig = Rig::new("normal");
    // Newer than the server has ever seen.
    let data = rig
        .refusal(
            "analyze.lsp_diagnostics",
            json!({"scope": "file", "path": rig.file(), "version": 5}),
        )
        .await;
    assert_eq!(
        data,
        json!({"kind": "lsp_stale", "requested": 5, "current": 1})
    );
    // Older: the file changed on disk, so the server moved to version 2.
    std::fs::write(rig.file(), "pub fn answer() -> u32 { 43 }\n").unwrap();
    let data = rig
        .refusal(
            "analyze.lsp_references",
            json!({"path": rig.file(), "line": 0, "column": 7, "version": 1}),
        )
        .await;
    assert_eq!(
        data,
        json!({"kind": "lsp_stale", "requested": 1, "current": 2})
    );
}

// ---- references ------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn lsp_references_returns_stamped_locations() {
    let rig = Rig::new("normal");
    let v = rig.ok("analyze.lsp_references", rig.refs_params()).await;
    assert_eq!(v["document_version"], 1);
    assert_snapshot(&v);
    let r = &v["references"][0];
    assert_eq!(r["path"], json!(rig.file()));
    assert_eq!(r["start"], json!({"line": 1, "column": 2}));
    assert_eq!(r["end"], json!({"line": 1, "column": 5}));
}

#[tokio::test(flavor = "multi_thread")]
async fn content_modified_is_retried_then_answers() {
    let rig = Rig::new("content_modified");
    let v = rig.ok("analyze.lsp_references", rig.refs_params()).await;
    assert_eq!(v["references"][0]["start"]["line"], 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn content_modified_that_never_clears_is_a_timeout() {
    let rig = Rig::with("content_modified_forever", |c| {
        c.request_timeout = Duration::from_millis(600);
    });
    let data = rig
        .refusal("analyze.lsp_references", rig.refs_params())
        .await;
    assert_eq!(data["kind"], "lsp_timeout", "{data}");
    // The server kept answering, so it is not counted as hung.
    assert_eq!(rig.manager.live_pids().await.len(), 1);
}

// ---- barrier ---------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn analysed_at_is_ready_once_the_version_is_published() {
    let rig = Rig::new("normal");
    let v = rig
        .ok(
            "analyze.lsp_analysed_at",
            json!({"path": rig.file(), "version": 1, "timeout_ms": 5000}),
        )
        .await;
    assert_eq!(v["state"], "ready", "{v}");
    assert_eq!(v["document_version"], 1);
    assert_snapshot(&v);
}

#[tokio::test(flavor = "multi_thread")]
async fn analysed_at_times_out_with_the_version_seen() {
    // The loading-time empty set at version 1 does not count.
    let rig = Rig::new("never_ready");
    let v = rig
        .ok(
            "analyze.lsp_analysed_at",
            json!({"path": rig.file(), "version": 1, "timeout_ms": 300}),
        )
        .await;
    assert_eq!(v["state"], "timed_out", "{v}");
    assert_eq!(v["version_seen"], Value::Null);
}

#[tokio::test(flavor = "multi_thread")]
async fn analysed_at_accepts_an_unversioned_publication_after_the_sync() {
    // The barrier uses the diagnostics rule: unversioned, published after sync.
    let rig = Rig::new("unversioned");
    let v = rig
        .ok(
            "analyze.lsp_analysed_at",
            json!({"path": rig.file(), "version": 1, "timeout_ms": 5000}),
        )
        .await;
    assert_eq!(v["state"], "ready", "{v}");
}

// ---- refusals --------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn disabled_flag_is_lsp_disabled_for_every_method() {
    let rig = Rig::with("normal", |c| c.enabled = false);
    for (method, params) in [
        ("analyze.lsp_diagnostics", rig.diag_params()),
        ("analyze.lsp_references", rig.refs_params()),
        (
            "analyze.lsp_analysed_at",
            json!({"path": rig.file(), "version": 1, "timeout_ms": 10}),
        ),
    ] {
        let data = rig.refusal(method, params).await;
        assert_eq!(data["kind"], "lsp_disabled", "{method}");
    }
    assert!(rig.manager.live_pids().await.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_paths_are_typed_refusals() {
    let rig = Rig::new("normal");
    let loose = rig.dir.path().join("loose.rs");
    std::fs::write(&loose, SOURCE).unwrap();
    let notes = rig.root().join("notes.txt");
    std::fs::write(&notes, "x").unwrap();
    for (path, kind) in [
        (loose, "lsp_outside_workspace"),
        (rig.root().join("src/missing.rs"), "lsp_file_not_found"),
        (notes, "lsp_unsupported_language"),
    ] {
        let data = rig
            .refusal(
                "analyze.lsp_references",
                json!({"path": path, "line": 0, "column": 0}),
            )
            .await;
        assert_eq!(data["kind"], kind, "{}", path.display());
        let data = rig
            .refusal(
                "analyze.lsp_diagnostics",
                json!({"scope": "file", "path": path}),
            )
            .await;
        assert_eq!(data["kind"], kind, "{}", path.display());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn malformed_params_are_invalid_params() {
    let rig = Rig::new("normal");
    for (method, params) in [
        (
            "analyze.lsp_references",
            json!({"path": "src/lib.rs", "line": 0, "column": 0}),
        ),
        ("analyze.lsp_diagnostics", json!({"scope": "file"})),
        (
            "analyze.lsp_references",
            json!({"path": rig.file(), "line": 0, "column": 0, "parent_event_id": "nope"}),
        ),
        // Past the MCP client's timeout minus its margin (240 s by default).
        (
            "analyze.lsp_analysed_at",
            json!({"path": rig.file(), "version": 1, "timeout_ms": 300_000}),
        ),
    ] {
        let r = rig.call(method, params.clone()).await;
        let err = r.error.unwrap_or_else(|| panic!("{params} succeeded"));
        assert_eq!(err.code, CODE_INVALID_PARAMS, "{params}: {err:?}");
    }
    // A well-formed parent_event_id is accepted.
    rig.ok(
        "analyze.lsp_references",
        json!({"path": rig.file(), "line": 0, "column": 7,
               "parent_event_id": "0192f0c8-4b1a-7c3e-9d2f-1a2b3c4d5e6f"}),
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_hung_server_is_reported_through_the_methods() {
    let rig = Rig::with("stall", |c| {
        c.request_timeout = Duration::from_millis(200);
        c.max_consecutive_timeouts = 2;
    });
    let first = rig
        .refusal("analyze.lsp_references", rig.refs_params())
        .await;
    assert_eq!(first["kind"], "lsp_timeout", "{first}");
    let second = rig
        .refusal("analyze.lsp_references", rig.refs_params())
        .await;
    assert_eq!(
        second,
        json!({"kind": "lsp_hung", "consecutive": 2}),
        "{second}"
    );
    // Inside the cool-off the recorded failure is replayed, not respawned.
    let third = rig
        .refusal("analyze.lsp_diagnostics", rig.diag_params())
        .await;
    assert_eq!(third["kind"], "lsp_hung", "{third}");
    assert!(rig.manager.live_pids().await.is_empty());
}

// ---- goldens ---------------------------------------------------------------

/// Replace the run-specific parts of a response with stable placeholders.
fn normalise(v: &Value, root: &Path) -> Value {
    let root = root.display().to_string();
    let mut text = serde_json::to_string_pretty(v).expect("encode");
    text = text.replace(&root, "<ROOT>");
    let mut v: Value = serde_json::from_str(&text).expect("decode");
    if v.get("snapshot").is_some() {
        v["snapshot"] = json!("<SNAPSHOT>");
    }
    v
}

fn check_golden(name: &str, actual: &Value) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name);
    let rendered = format!(
        "{}\n",
        serde_json::to_string_pretty(actual).expect("encode")
    );
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("golden dir");
        std::fs::write(&path, &rendered).expect("write golden");
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e}; run with UPDATE_GOLDEN=1", path.display()));
    // Compared as JSON values: key order depends on serde_json's features in
    // a given build and is not part of the wire contract.
    let expected: Value = serde_json::from_str(&expected).expect("golden is JSON");
    assert_eq!(
        actual, &expected,
        "{name} drifted; rerun with UPDATE_GOLDEN=1 if intended\nactual:\n{rendered}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn lsp_diagnostics_response_matches_its_golden() {
    let rig = Rig::new("normal");
    let v = rig.ok("analyze.lsp_diagnostics", rig.diag_params()).await;
    check_golden("lsp_diagnostics.json", &normalise(&v, &rig.root()));
}

#[tokio::test(flavor = "multi_thread")]
async fn lsp_references_response_matches_its_golden() {
    let rig = Rig::new("normal");
    let v = rig.ok("analyze.lsp_references", rig.refs_params()).await;
    check_golden("lsp_references.json", &normalise(&v, &rig.root()));
}
