//! Transport-parity contract tests for the registry RPC methods (#6288 slice 5).
//!
//! Why these are PARITY tests and not "the socket answers JSON": the slice's
//! claim is that a route reached over the socket and the same route reached over
//! HTTP give the same answer AND leave the same state behind. A test that only
//! drives the RPC side proves the method exists; it cannot fail when the two
//! transports drift, which is the failure this slice exists to prevent. So every
//! `parity_*` case builds the axum router AND the RPC router from ONE
//! `Arc<DaemonState>` and compares.
//!
//! For a WRITE route that is not enough: two calls that return equal JSON could
//! still have persisted different things, or one could have persisted nothing.
//! So each write route's case reads the store back afterwards and asserts BOTH
//! records are there — see `assert_same_after_write`'s callers.
//!
//! ## The comparison allowlist
//!
//! Server-assigned identity fields are normalised before comparing, because two
//! calls of the same code differ there by construction, not because the
//! transports disagree: `id` and `created_at` on a Deliverable/Milestone, and
//! `code`/`expires_in_seconds` on a freshly minted pairing code. Nothing else is
//! excused.
//!
//! Test: this file IS the test module for [`super`].

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;
use trusty_common::uds::server::{
    CODE_INVALID_PARAMS, CODE_METHOD_NOT_FOUND, RpcError, RpcResponse, RpcRouter,
};

use super::{METHODS, register};
use crate::core::paths::FrameworkPaths;
use crate::daemon::api;
use crate::daemon::error::{
    CODE_CONFLICT, CODE_NOT_FOUND, CODE_UNAVAILABLE, CODE_UPSTREAM_FAILED, DaemonError,
};
use crate::daemon::state::DaemonState;

/// One daemon state rooted at an empty temp directory, plus the directory.
///
/// Why hermetic: a developer machine with a live `overseer.toml` or a real
/// project registry would let a store read return records this suite never
/// wrote. The caller holds the `TempDir` for the test's life.
fn hermetic() -> (Arc<DaemonState>, TempDir) {
    let dir = tempfile::tempdir().expect("temp dir for hermetic DaemonState");
    let paths = FrameworkPaths::under(dir.path());
    (Arc::new(DaemonState::with_paths(&paths)), dir)
}

/// RAII guard restoring `$HOME` on drop, panic included (#6671).
///
/// A copy of `session_manager::naming_tests`'s guard rather than a shared one:
/// module-private visibility does not cross sibling module trees, and several
/// other test modules in this crate carry the same copy for the same reason.
/// Callers MUST be `#[serial_test::serial]` — the guard makes the mutation
/// reversible, not atomic, and other tests in this binary write `$HOME`.
/// Test: used by [`hermetic_isolated_home`]'s callers.
struct HomeGuard(Option<String>);

impl Drop for HomeGuard {
    fn drop(&mut self) {
        // SAFETY: paired with `#[serial_test::serial]` — no other thread reads
        // or writes the environment concurrently.
        match self.0 {
            Some(ref p) => unsafe { std::env::set_var("HOME", p) },
            None => unsafe { std::env::remove_var("HOME") },
        }
    }
}

/// [`hermetic`], plus a scratch `$HOME` for the caller's scope (#6671).
///
/// Why: a hermetic ROOT is not a hermetic REGISTRY.
/// `DaemonState::project_registry()` seeds itself from
/// `TrustyToolsConfig::load()`, which resolves
/// `~/.trusty-tools/trusty-mpm/config.yaml` through `$HOME`, so the four callers
/// below read the DEVELOPER's registered projects — they asserted
/// `project_count == 1` on a machine answering 23. Same defect #6671 reports for
/// five integration targets, here in the lib.
/// What: returns [`hermetic`]'s state and root, the scratch home, and a
/// [`HomeGuard`]. The caller binds all four, so `$HOME` is restored when the
/// test returns or panics, and restored BEFORE the scratch home is removed —
/// tuple bindings drop in reverse order, so the guard goes first.
/// Test: `parity_manager_status_agrees_across_transports`,
/// `parity_projects_registry_list_agrees_across_transports`,
/// `parity_projects_registry_register_agrees_across_transports`,
/// `rpc_projects_registry_register_rejects_a_blank_name`.
fn hermetic_isolated_home() -> (Arc<DaemonState>, TempDir, TempDir, HomeGuard) {
    let home = tempfile::tempdir().expect("scratch $HOME");
    let prior = std::env::var("HOME").ok();
    // SAFETY: serialized via `#[serial_test::serial]` on every caller.
    unsafe { std::env::set_var("HOME", home.path()) };
    let guard = HomeGuard(prior);
    let (state, dir) = hermetic();
    (state, dir, home, guard)
}

/// The RPC router this slice registers, over `state`.
fn rpc_router(state: &Arc<DaemonState>) -> RpcRouter {
    register(RpcRouter::new(), state)
}

/// Force the manager's inference seam into "no provider configured".
///
/// Why (#1523): a hermetic ROOT is not a hermetic ENVIRONMENT.
/// `ManagerInference` resolves a provider from ambient env, so a machine with
/// `OPENROUTER_API_KEY` set builds a REAL adapter, and the digest and chat cases
/// would call a live model instead of taking the degrade path they assert.
/// `set_unconfigured` is the seam that exists for exactly this, and it makes
/// both transports take the SAME leg — which is what a parity case needs.
fn without_a_provider(state: &Arc<DaemonState>) {
    state.manager_state().inference().set_unconfigured();
}

/// Drive one HTTP request through the real daemon router and decode the answer.
///
/// Why the real router rather than calling a handler directly: the parity claim
/// is about the ROUTE, so the path, the method, and the extractors all have to
/// participate — that decoding is exactly what the socket has to reproduce.
async fn http(
    state: &Arc<DaemonState>,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let request = Request::builder().method(method).uri(uri);
    let request = match body {
        Some(v) => request
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&v).expect("encode body")))
            .expect("build request"),
        None => request.body(Body::empty()).expect("build request"),
    };

    let response = api::router(Arc::clone(state))
        .oneshot(request)
        .await
        .expect("the router must answer");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read the response body");
    // A plain-text or bodyless refusal is not JSON; report it as a string (or
    // null) rather than failing the decode, so an error-parity case can still
    // compare the status.
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
    };
    (status, value)
}

/// Dispatch one JSON-RPC call against `router` and return the whole frame.
async fn rpc_frame(router: &RpcRouter, method: &str, params: Value) -> Value {
    let frame = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
    let response: RpcResponse = router
        .dispatch(&serde_json::to_vec(&frame).expect("encode the request frame"))
        .await;
    serde_json::to_value(response).expect("the response frame must serialise")
}

/// The `result` half of a call that must succeed.
async fn rpc_ok(router: &RpcRouter, method: &str, params: Value) -> Value {
    let frame = rpc_frame(router, method, params).await;
    assert!(
        frame.get("error").is_none(),
        "{method} must succeed, got {frame}"
    );
    frame
        .get("result")
        .cloned()
        .unwrap_or_else(|| panic!("{method} answered no result: {frame}"))
}

/// The `error` half of a call that must be refused.
async fn rpc_err(router: &RpcRouter, method: &str, params: Value) -> Value {
    let frame = rpc_frame(router, method, params).await;
    frame
        .get("error")
        .cloned()
        .unwrap_or_else(|| panic!("{method} must be refused, got {frame}"))
}

/// Assert the two transports answered the same JSON for one route.
fn assert_same(method: &str, mut http_body: Value, mut rpc_result: Value, drop_fields: &[&str]) {
    for field in drop_fields {
        if let Some(map) = http_body.as_object_mut() {
            map.remove(*field);
        }
        if let Some(map) = rpc_result.as_object_mut() {
            map.remove(*field);
        }
    }
    assert_eq!(
        http_body, rpc_result,
        "{method} must answer identically over HTTP and the socket"
    );
}

// ── The method table ─────────────────────────────────────────────────────────

/// Why: the slice-7 client swap and trusty-console will dial these names by
/// literal, with no compile-time link to the registrations. Pinning the set here
/// turns a rename into a failing assertion rather than a consumer that silently
/// reports `method_not_found`.
/// Test: this function IS the test.
#[tokio::test]
async fn rpc_router_registers_every_documented_method() {
    let (state, _dir) = hermetic();
    let router = rpc_router(&state);
    let registered: Vec<&str> = router.method_names().collect();

    let mut documented: Vec<&str> = METHODS.to_vec();
    documented.sort_unstable();

    assert_eq!(
        registered, documented,
        "the router and METHODS must name the same set"
    );
    assert_eq!(
        METHODS.len(),
        29,
        "slice 5 owns twenty-nine routes; a new one needs a row in registry.rs's table too"
    );
}

/// Why: slice 5 mounted four `mpm.bus.*` methods here, and the 2026-09-05 owner
/// ruling retired them — trusty-console hosts the only event bus, and mpm
/// publishes to it through the `trusty-common` PushClient (#6849). A later slice
/// re-mounting one would put a second bus back on the socket without anyone
/// noticing, so the absence is asserted rather than left to review.
/// Test: this function IS the test.
#[tokio::test]
async fn rpc_router_serves_no_bus_method() {
    let (state, _dir) = hermetic();
    let router = rpc_router(&state);

    assert!(
        !router.method_names().any(|m| m.starts_with("mpm.bus.")),
        "no bus method may be registered: {:?}",
        router.method_names().collect::<Vec<_>>()
    );
    assert!(
        !METHODS.iter().any(|m| m.starts_with("mpm.bus.")),
        "no bus method may be documented in METHODS"
    );
    assert!(
        router.stream_names().next().is_none(),
        "this module registers no stream methods"
    );
}

/// Why the dispatch and not just the name list: a consumer that still dials a
/// retired name must get a refusal it can branch on, not a hang or a silent
/// empty result. `method_not_found` is that refusal.
/// Test: this function IS the test.
#[tokio::test]
async fn rpc_every_retired_bus_method_answers_method_not_found() {
    let (state, _dir) = hermetic();
    let router = rpc_router(&state);

    for method in [
        "mpm.bus.register",
        "mpm.bus.deregister",
        "mpm.bus.list",
        "mpm.bus.publish",
    ] {
        let error = rpc_err(&router, method, Value::Null).await;
        assert_eq!(
            error["code"],
            json!(CODE_METHOD_NOT_FOUND),
            "{method} must be refused as unknown: {error}"
        );
    }
}

/// Why: `params` is absent on a well-formed no-argument call, and a plain unit
/// struct refuses `null` — which would make every listing fail with
/// `invalid_params`. [`super::NoParams`] exists to prevent that.
/// Test: this function IS the test.
#[tokio::test]
async fn rpc_projects_list_answers_with_no_params() {
    let (state, _dir) = hermetic();
    let result = rpc_ok(&rpc_router(&state), "mpm.projects.list", Value::Null).await;
    assert!(result["projects"].is_array(), "{result}");
}

// ── Legacy `/projects*` ──────────────────────────────────────────────────────

/// Why the register calls come first: an empty registry would make the two
/// listings trivially equal, which proves nothing about the data crossing.
/// Test: this function IS the test.
#[tokio::test]
async fn parity_projects_list_agrees_across_transports() {
    let (state, dir) = hermetic();
    let a = dir.path().join("alpha");
    std::fs::create_dir_all(&a).expect("mkdir");
    state.register_project(a);

    let (status, body) = http(&state, "GET", "/projects", None).await;
    assert_eq!(status, StatusCode::OK);
    let result = rpc_ok(&rpc_router(&state), "mpm.projects.list", Value::Null).await;
    assert_eq!(
        body["projects"].as_array().map(Vec::len),
        Some(1),
        "the registration must reach the listing: {body}"
    );
    assert_same("mpm.projects.list", body, result, &[]);
}

/// Why this asserts the STORE and not just the two answers: register is a write,
/// and two equal responses could both have persisted nothing.
/// Test: this function IS the test.
#[tokio::test]
async fn parity_projects_register_agrees_across_transports() {
    let (state, dir) = hermetic();
    let over_http = dir.path().join("via-http");
    let over_rpc = dir.path().join("via-rpc");
    std::fs::create_dir_all(&over_http).expect("mkdir");
    std::fs::create_dir_all(&over_rpc).expect("mkdir");

    let (status, body) = http(
        &state,
        "POST",
        "/projects",
        Some(json!({ "path": over_http })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let result = rpc_ok(
        &rpc_router(&state),
        "mpm.projects.register",
        json!({ "path": over_rpc }),
    )
    .await;

    // The two answers differ only in the path each announced, so compare the
    // shape by key set and then prove BOTH writes landed.
    assert_eq!(
        body.as_object().map(|m| m.keys().collect::<Vec<_>>()),
        result.as_object().map(|m| m.keys().collect::<Vec<_>>()),
        "both transports must answer the same ProjectInfo shape"
    );
    assert!(
        state.project(&over_http).is_some(),
        "the HTTP register must have persisted"
    );
    assert!(
        state.project(&over_rpc).is_some(),
        "the RPC register must have persisted the same way"
    );
}

/// Test: this function IS the test.
#[tokio::test]
async fn parity_projects_current_agrees_across_transports() {
    let (state, dir) = hermetic();
    let path = dir.path().join("alpha");
    std::fs::create_dir_all(&path).expect("mkdir");
    state.register_project(path.clone());

    let uri = format!("/projects/current?path={}", path.display());
    let (status, body) = http(&state, "GET", &uri, None).await;
    assert_eq!(status, StatusCode::OK);
    let result = rpc_ok(
        &rpc_router(&state),
        "mpm.projects.current",
        json!({ "path": path }),
    )
    .await;
    assert_same("mpm.projects.current", body, result, &[]);
}

/// Why: an unregistered path is this route's only failure, and the two
/// transports must agree it is a not-found rather than an empty answer.
/// Test: this function IS the test.
#[tokio::test]
async fn rpc_projects_current_unregistered_path_is_not_found() {
    let (state, dir) = hermetic();
    let path = dir.path().join("never-registered");

    let uri = format!("/projects/current?path={}", path.display());
    let (status, _) = http(&state, "GET", &uri, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let error = rpc_err(
        &rpc_router(&state),
        "mpm.projects.current",
        json!({ "path": path }),
    )
    .await;
    assert_eq!(error["code"], json!(CODE_NOT_FOUND), "{error}");
}

/// Why serial: discovery reads `~/.claude/projects/`, and other tests in this
/// binary move `$HOME` process-wide. One landing between the two calls would
/// make the HTTP answer read a temp home and the socket answer the real one,
/// failing the comparison while proving nothing about the transports.
/// Test: this function IS the test.
#[serial_test::serial]
#[tokio::test]
async fn parity_projects_discover_agrees_across_transports() {
    let (state, _dir) = hermetic();
    let (status, body) = http(&state, "GET", "/projects/discover", None).await;
    assert_eq!(status, StatusCode::OK);
    let result = rpc_ok(&rpc_router(&state), "mpm.projects.discover", Value::Null).await;
    assert_same("mpm.projects.discover", body, result, &[]);
}

// ── Registry-B `/api/v1/projects*` ───────────────────────────────────────────

/// Register one registry-B project directly through the store.
async fn seed_project(state: &Arc<DaemonState>, name: &str) {
    let registry = state.project_registry().await;
    registry
        .register(crate::project::Project {
            name: name.to_string(),
            repo_url: format!("https://example.invalid/{name}"),
            default_branch: "main".to_string(),
            stack_hint: None,
            tags: Vec::new(),
            description: None,
            gh_user: None,
            gh_account: None,
            github: None,
            commit_name: None,
            commit_email: None,
            worktree: None,
        })
        .await
        .expect("seed the project registry");
}

/// Test: this function IS the test.
#[tokio::test]
// #6671: reads the project registry, which seeds from `$HOME`.
#[serial_test::serial]
async fn parity_projects_registry_list_agrees_across_transports() {
    let (state, _dir, _home_dir, _home) = hermetic_isolated_home();
    seed_project(&state, "alpha").await;

    let (status, body) = http(&state, "GET", "/api/v1/projects", None).await;
    assert_eq!(status, StatusCode::OK);
    let result = rpc_ok(
        &rpc_router(&state),
        "mpm.projects.registry.list",
        Value::Null,
    )
    .await;
    assert_eq!(body["count"], json!(1), "the seed must reach the listing");
    assert_same("mpm.projects.registry.list", body, result, &[]);
}

/// Why the store read at the end: register is a write, and this is the file the
/// registry-B surface persists to. Two equal `201` bodies would not prove either
/// call reached it.
/// Test: this function IS the test.
#[tokio::test]
// #6671: reads the project registry, which seeds from `$HOME`.
#[serial_test::serial]
async fn parity_projects_registry_register_agrees_across_transports() {
    let (state, _dir, _home_dir, _home) = hermetic_isolated_home();

    let body_for = |name: &str| {
        json!({
            "name": name,
            "repo_url": "https://example.invalid/repo",
            "description": "seeded by the parity suite",
        })
    };

    let (status, http_body) = http(
        &state,
        "POST",
        "/api/v1/projects",
        Some(body_for("via-http")),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let rpc_body = rpc_ok(
        &rpc_router(&state),
        "mpm.projects.registry.register",
        body_for("via-rpc"),
    )
    .await;

    // Same record shape, differing only in the name each call chose.
    assert_same(
        "mpm.projects.registry.register",
        http_body,
        rpc_body,
        &["name"],
    );

    // Both writes must be in the ONE registry file.
    let registry = state.project_registry().await;
    let mut names: Vec<String> = registry
        .list()
        .await
        .expect("list the registry")
        .into_iter()
        .map(|p| p.name)
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec!["via-http".to_string(), "via-rpc".to_string()],
        "both transports must have persisted into the same registry"
    );
}

/// Test: this function IS the test.
#[tokio::test]
async fn parity_projects_registry_get_agrees_across_transports() {
    let (state, _dir) = hermetic();
    seed_project(&state, "alpha").await;

    let (status, body) = http(&state, "GET", "/api/v1/projects/alpha", None).await;
    assert_eq!(status, StatusCode::OK);
    let result = rpc_ok(
        &rpc_router(&state),
        "mpm.projects.registry.get",
        json!({ "name": "alpha" }),
    )
    .await;
    assert_same("mpm.projects.registry.get", body, result, &[]);
}

/// Why: an unknown project is a `404` with a plain-text body over HTTP, which
/// has no JSON-RPC counterpart — the code has to carry the whole distinction.
/// Test: this function IS the test.
#[tokio::test]
async fn rpc_projects_registry_get_unknown_project_is_not_found() {
    let (state, _dir) = hermetic();

    let (status, body) = http(&state, "GET", "/api/v1/projects/nope", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        body,
        json!("project nope not found"),
        "the plain-text HTTP body must be unchanged by the extraction"
    );

    let error = rpc_err(
        &rpc_router(&state),
        "mpm.projects.registry.get",
        json!({ "name": "nope" }),
    )
    .await;
    assert_eq!(error["code"], json!(CODE_NOT_FOUND), "{error}");
}

/// Why two different fields: a PATCH that set the same field twice could pass
/// while one transport silently no-opped. Each call edits its own project and
/// the store is read back for both.
/// Test: this function IS the test.
#[tokio::test]
async fn parity_projects_registry_patch_agrees_across_transports() {
    let (state, _dir) = hermetic();
    seed_project(&state, "via-http").await;
    seed_project(&state, "via-rpc").await;

    let (status, http_body) = http(
        &state,
        "PATCH",
        "/api/v1/projects/via-http",
        Some(json!({ "description": "patched" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let rpc_body = rpc_ok(
        &rpc_router(&state),
        "mpm.projects.registry.patch",
        json!({ "name": "via-rpc", "description": "patched" }),
    )
    .await;
    assert_same(
        "mpm.projects.registry.patch",
        http_body,
        rpc_body,
        &["name", "repo_url"],
    );

    let registry = state.project_registry().await;
    for name in ["via-http", "via-rpc"] {
        let stored = registry.get(name).await.expect("the project must persist");
        assert_eq!(
            stored.description.as_deref(),
            Some("patched"),
            "{name}: the patch must be in the store, not only in the answer"
        );
    }
}

/// Why the RPC half asserts a BLANK `repo_url` and not the rename: over HTTP a
/// rename is a body `name` disagreeing with the path `name`, and the params
/// struct flattens the body around ONE `name` field — so the two can never
/// disagree on the socket, and the rename rejection is unreachable there by
/// construction rather than by a check. That is strictly stronger than the HTTP
/// guard, not weaker. What both transports CAN express is a blank required
/// field, which runs through the same validation, before the store is touched.
/// Test: this function IS the test.
#[tokio::test]
async fn rpc_projects_registry_patch_rejects_a_blank_required_field() {
    let (state, _dir) = hermetic();
    seed_project(&state, "alpha").await;

    // The HTTP-only rename guard still holds.
    let (status, _) = http(
        &state,
        "PATCH",
        "/api/v1/projects/alpha",
        Some(json!({ "name": "renamed" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _) = http(
        &state,
        "PATCH",
        "/api/v1/projects/alpha",
        Some(json!({ "repo_url": "  " })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let error = rpc_err(
        &rpc_router(&state),
        "mpm.projects.registry.patch",
        json!({ "name": "alpha", "repo_url": "  " }),
    )
    .await;
    assert_eq!(error["code"], json!(CODE_INVALID_PARAMS), "{error}");

    let registry = state.project_registry().await;
    let stored = registry.get("alpha").await.expect("still registered");
    assert_eq!(
        stored.repo_url, "https://example.invalid/alpha",
        "a rejected patch must leave the record untouched on both paths"
    );
}

/// Why: a blank name is rejected before the store is touched on both paths.
/// Test: this function IS the test.
#[tokio::test]
// #6671: reads the project registry, which seeds from `$HOME`.
#[serial_test::serial]
async fn rpc_projects_registry_register_rejects_a_blank_name() {
    let (state, _dir, _home_dir, _home) = hermetic_isolated_home();
    let payload = json!({ "name": "   ", "repo_url": "https://example.invalid/r" });

    let (status, _) = http(&state, "POST", "/api/v1/projects", Some(payload.clone())).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let error = rpc_err(
        &rpc_router(&state),
        "mpm.projects.registry.register",
        payload,
    )
    .await;
    assert_eq!(error["code"], json!(CODE_INVALID_PARAMS), "{error}");

    let registry = state.project_registry().await;
    assert!(
        registry.list().await.expect("list").is_empty(),
        "a rejected register must leave the registry untouched on both paths"
    );
}

/// Test: this function IS the test.
#[tokio::test]
async fn parity_project_status_agrees_across_transports() {
    let (state, _dir) = hermetic();
    seed_project(&state, "alpha").await;

    let (status, body) = http(&state, "GET", "/api/v1/projects/alpha/status", None).await;
    assert_eq!(status, StatusCode::OK);
    let result = rpc_ok(
        &rpc_router(&state),
        "mpm.projects.status",
        json!({ "name": "alpha" }),
    )
    .await;
    assert_same("mpm.projects.status", body, result, &[]);
}

/// Test: this function IS the test.
#[tokio::test]
async fn rpc_project_status_unknown_project_is_not_found() {
    let (state, _dir) = hermetic();

    let (status, _) = http(&state, "GET", "/api/v1/projects/nope/status", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let error = rpc_err(
        &rpc_router(&state),
        "mpm.projects.status",
        json!({ "name": "nope" }),
    )
    .await;
    assert_eq!(error["code"], json!(CODE_NOT_FOUND), "{error}");
}

// ── Deliverables and Milestones ──────────────────────────────────────────────

/// The two fields the server assigns per record, which two calls of the same
/// code differ on by construction.
const RECORD_IDENTITY: &[&str] = &["id", "created_at"];

/// A minimal create body for a Deliverable.
fn deliverable_body(name: &str) -> Value {
    json!({ "name": name, "kind": "feature", "estimated_effort": "M" })
}

/// A minimal create body for a Milestone.
fn milestone_body(name: &str) -> Value {
    json!({ "name": name, "target_date": "2030-01-01T00:00:00Z" })
}

/// Why the store read: create is a write into the deliverable store, and two
/// equal `201` bodies would not prove either call reached it.
/// Test: this function IS the test.
#[tokio::test]
async fn parity_deliverable_create_agrees_across_transports() {
    let (state, _dir) = hermetic();

    let (status, http_body) = http(
        &state,
        "POST",
        "/api/v1/projects/alpha/deliverables",
        Some(deliverable_body("shared")),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let rpc_body = rpc_ok(
        &rpc_router(&state),
        "mpm.deliverables.create",
        json!({ "project": "alpha", "name": "shared", "kind": "feature",
                "estimated_effort": "M" }),
    )
    .await;
    assert_same(
        "mpm.deliverables.create",
        http_body,
        rpc_body,
        RECORD_IDENTITY,
    );

    let mgr = state.deliverable_manager().await;
    let stored = mgr
        .deliverables_by_project("alpha")
        .await
        .expect("read the deliverable store");
    assert_eq!(
        stored.len(),
        2,
        "both transports must have persisted into the same store"
    );
}

/// Test: this function IS the test.
#[tokio::test]
async fn parity_deliverable_list_agrees_across_transports() {
    let (state, _dir) = hermetic();
    http(
        &state,
        "POST",
        "/api/v1/projects/alpha/deliverables",
        Some(deliverable_body("seeded")),
    )
    .await;

    let (status, body) = http(&state, "GET", "/api/v1/projects/alpha/deliverables", None).await;
    assert_eq!(status, StatusCode::OK);
    let result = rpc_ok(
        &rpc_router(&state),
        "mpm.deliverables.list",
        json!({ "project": "alpha" }),
    )
    .await;
    assert_eq!(
        body["deliverables"].as_array().map(Vec::len),
        Some(1),
        "the seed must reach the listing: {body}"
    );
    assert_same("mpm.deliverables.list", body, result, &[]);
}

/// Why the filter is passed explicitly: it proves the ARGUMENT survives the
/// transport change, not only the unfiltered default.
/// Test: this function IS the test.
#[tokio::test]
async fn parity_deliverable_list_status_filter_agrees_across_transports() {
    let (state, _dir) = hermetic();
    http(
        &state,
        "POST",
        "/api/v1/projects/alpha/deliverables",
        Some(deliverable_body("seeded")),
    )
    .await;

    let (status, body) = http(
        &state,
        "GET",
        "/api/v1/projects/alpha/deliverables?status=blocked",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["deliverables"].as_array().map(Vec::len),
        Some(0),
        "the filter must exclude the proposed record: {body}"
    );
    let result = rpc_ok(
        &rpc_router(&state),
        "mpm.deliverables.list",
        json!({ "project": "alpha", "status": "blocked" }),
    )
    .await;
    assert_same("mpm.deliverables.list", body, result, &[]);
}

/// Create one Deliverable over HTTP and return its id.
async fn seed_deliverable(state: &Arc<DaemonState>, project: &str, name: &str) -> String {
    let (_, body) = http(
        state,
        "POST",
        &format!("/api/v1/projects/{project}/deliverables"),
        Some(deliverable_body(name)),
    )
    .await;
    body["id"].as_str().expect("a created id").to_string()
}

/// Test: this function IS the test.
#[tokio::test]
async fn parity_deliverable_get_agrees_across_transports() {
    let (state, _dir) = hermetic();
    let id = seed_deliverable(&state, "alpha", "seeded").await;

    let (status, body) = http(
        &state,
        "GET",
        &format!("/api/v1/projects/alpha/deliverables/{id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let result = rpc_ok(
        &rpc_router(&state),
        "mpm.deliverables.get",
        json!({ "project": "alpha", "id": id }),
    )
    .await;
    assert_same("mpm.deliverables.get", body, result, &[]);
}

/// Why two records: a patch is a write, so each transport edits its own and the
/// store is read back for both.
/// Test: this function IS the test.
#[tokio::test]
async fn parity_deliverable_patch_agrees_across_transports() {
    let (state, _dir) = hermetic();
    let via_http = seed_deliverable(&state, "alpha", "via-http").await;
    let via_rpc = seed_deliverable(&state, "alpha", "via-rpc").await;

    let (status, http_body) = http(
        &state,
        "PATCH",
        &format!("/api/v1/projects/alpha/deliverables/{via_http}"),
        Some(json!({ "status": "in-progress" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let rpc_body = rpc_ok(
        &rpc_router(&state),
        "mpm.deliverables.patch",
        json!({ "project": "alpha", "id": via_rpc, "status": "in-progress" }),
    )
    .await;
    assert_same(
        "mpm.deliverables.patch",
        http_body,
        rpc_body,
        &["id", "created_at", "name"],
    );

    let mgr = state.deliverable_manager().await;
    for id in [&via_http, &via_rpc] {
        let stored = mgr.get_deliverable(id).await.expect("the record persists");
        assert_eq!(
            stored.status,
            crate::deliverable::DeliverableStatus::InProgress,
            "{id}: the transition must be in the store, not only in the answer"
        );
    }
}

/// Why: §10.3 rejects `proposed → complete`, and that rejection runs inside the
/// same write-locked closure on both transports. It is the sharpest available
/// proof that the state machine did not move.
/// Test: this function IS the test.
#[tokio::test]
async fn rpc_deliverable_patch_rejects_an_illegal_transition() {
    let (state, _dir) = hermetic();
    let id = seed_deliverable(&state, "alpha", "seeded").await;

    let error = rpc_err(
        &rpc_router(&state),
        "mpm.deliverables.patch",
        json!({ "project": "alpha", "id": id, "status": "complete" }),
    )
    .await;
    assert_eq!(error["code"], json!(CODE_CONFLICT), "{error}");

    let mgr = state.deliverable_manager().await;
    let stored = mgr.get_deliverable(&id).await.expect("the record persists");
    assert_eq!(
        stored.status,
        crate::deliverable::DeliverableStatus::Proposed,
        "a rejected transition must leave the record untouched over the socket too"
    );
}

/// Test: this function IS the test.
#[tokio::test]
async fn rpc_deliverable_create_rejects_a_blank_name() {
    let (state, _dir) = hermetic();

    let (status, _) = http(
        &state,
        "POST",
        "/api/v1/projects/alpha/deliverables",
        Some(deliverable_body("   ")),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let error = rpc_err(
        &rpc_router(&state),
        "mpm.deliverables.create",
        json!({ "project": "alpha", "name": "   ", "kind": "feature",
                "estimated_effort": "M" }),
    )
    .await;
    assert_eq!(error["code"], json!(CODE_INVALID_PARAMS), "{error}");

    let mgr = state.deliverable_manager().await;
    assert!(
        mgr.deliverables_by_project("alpha")
            .await
            .expect("read")
            .is_empty(),
        "a rejected create must persist nothing on either path"
    );
}

/// Test: this function IS the test.
#[tokio::test]
async fn rpc_deliverable_get_wrong_project_is_not_found() {
    let (state, _dir) = hermetic();
    let id = seed_deliverable(&state, "alpha", "seeded").await;

    let (status, _) = http(
        &state,
        "GET",
        &format!("/api/v1/projects/beta/deliverables/{id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let error = rpc_err(
        &rpc_router(&state),
        "mpm.deliverables.get",
        json!({ "project": "beta", "id": id }),
    )
    .await;
    assert_eq!(error["code"], json!(CODE_NOT_FOUND), "{error}");
}

/// Test: this function IS the test.
#[tokio::test]
async fn parity_milestone_create_agrees_across_transports() {
    let (state, _dir) = hermetic();

    let (status, http_body) = http(
        &state,
        "POST",
        "/api/v1/projects/alpha/milestones",
        Some(milestone_body("shared")),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let rpc_body = rpc_ok(
        &rpc_router(&state),
        "mpm.milestones.create",
        json!({ "project": "alpha", "name": "shared",
                "target_date": "2030-01-01T00:00:00Z" }),
    )
    .await;
    assert_same(
        "mpm.milestones.create",
        http_body,
        rpc_body,
        RECORD_IDENTITY,
    );

    let mgr = state.deliverable_manager().await;
    assert_eq!(
        mgr.milestones_by_project("alpha")
            .await
            .expect("read the milestone store")
            .len(),
        2,
        "both transports must have persisted into the same store"
    );
}

/// Test: this function IS the test.
#[tokio::test]
async fn parity_milestone_list_agrees_across_transports() {
    let (state, _dir) = hermetic();
    http(
        &state,
        "POST",
        "/api/v1/projects/alpha/milestones",
        Some(milestone_body("seeded")),
    )
    .await;

    let (status, body) = http(&state, "GET", "/api/v1/projects/alpha/milestones", None).await;
    assert_eq!(status, StatusCode::OK);
    let result = rpc_ok(
        &rpc_router(&state),
        "mpm.milestones.list",
        json!({ "project": "alpha" }),
    )
    .await;
    assert_eq!(
        body["milestones"].as_array().map(Vec::len),
        Some(1),
        "{body}"
    );
    assert_same("mpm.milestones.list", body, result, &[]);
}

/// Create one Milestone over HTTP and return its id.
async fn seed_milestone(state: &Arc<DaemonState>, project: &str, name: &str) -> String {
    let (_, body) = http(
        state,
        "POST",
        &format!("/api/v1/projects/{project}/milestones"),
        Some(milestone_body(name)),
    )
    .await;
    body["id"].as_str().expect("a created id").to_string()
}

/// Test: this function IS the test.
#[tokio::test]
async fn parity_milestone_get_agrees_across_transports() {
    let (state, _dir) = hermetic();
    let id = seed_milestone(&state, "alpha", "seeded").await;

    let (status, body) = http(
        &state,
        "GET",
        &format!("/api/v1/projects/alpha/milestones/{id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let result = rpc_ok(
        &rpc_router(&state),
        "mpm.milestones.get",
        json!({ "project": "alpha", "id": id }),
    )
    .await;
    assert_same("mpm.milestones.get", body, result, &[]);
}

/// Test: this function IS the test.
#[tokio::test]
async fn parity_milestone_patch_agrees_across_transports() {
    let (state, _dir) = hermetic();
    let via_http = seed_milestone(&state, "alpha", "via-http").await;
    let via_rpc = seed_milestone(&state, "alpha", "via-rpc").await;

    let (status, http_body) = http(
        &state,
        "PATCH",
        &format!("/api/v1/projects/alpha/milestones/{via_http}"),
        Some(json!({ "description": "patched" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let rpc_body = rpc_ok(
        &rpc_router(&state),
        "mpm.milestones.patch",
        json!({ "project": "alpha", "id": via_rpc, "description": "patched" }),
    )
    .await;
    assert_same(
        "mpm.milestones.patch",
        http_body,
        rpc_body,
        &["id", "created_at", "name"],
    );

    let mgr = state.deliverable_manager().await;
    for id in [&via_http, &via_rpc] {
        let stored = mgr.get_milestone(id).await.expect("the record persists");
        assert_eq!(
            stored.description, "patched",
            "{id}: the patch must be in the store, not only in the answer"
        );
    }
}

// ── L3 manager ───────────────────────────────────────────────────────────────

/// Test: this function IS the test.
#[tokio::test]
async fn parity_manager_version_agrees_across_transports() {
    let (state, _dir) = hermetic();
    let (status, body) = http(&state, "GET", "/api/v1/manager/version", None).await;
    assert_eq!(status, StatusCode::OK);
    let result = rpc_ok(&rpc_router(&state), "mpm.manager.version", Value::Null).await;
    assert_same("mpm.manager.version", body, result, &[]);
}

/// Test: this function IS the test.
#[tokio::test]
// #6671: reads the project registry, which seeds from `$HOME`.
#[serial_test::serial]
async fn parity_manager_status_agrees_across_transports() {
    let (state, _dir, _home_dir, _home) = hermetic_isolated_home();
    seed_project(&state, "alpha").await;

    let (status, body) = http(&state, "GET", "/api/v1/manager/status", None).await;
    assert_eq!(status, StatusCode::OK);
    let result = rpc_ok(&rpc_router(&state), "mpm.manager.status", Value::Null).await;
    assert_eq!(body["project_count"], json!(1), "{body}");
    assert_same("mpm.manager.status", body, result, &[]);
}

/// Why the HTTP status is 503 and the RPC call still succeeds: the degrade leg
/// carries a COMPLETE body — the deterministic narrative plus the rollup — which
/// a JSON-RPC error frame cannot hold. Over the socket the body is the result,
/// and the `error` field already on it carries the distinction. This asserts
/// both halves of that claim at once.
/// Test: this function IS the test.
#[tokio::test]
async fn parity_manager_digest_agrees_across_transports() {
    let (state, _dir) = hermetic();
    without_a_provider(&state);

    let (status, body) = http(&state, "GET", "/api/v1/manager/digest", None).await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "with the provider forced off, this is the degrade leg"
    );
    let result = rpc_ok(&rpc_router(&state), "mpm.manager.digest", Value::Null).await;
    assert_eq!(
        result["error"],
        json!("inference_unavailable"),
        "the body must carry the distinction the status carried: {result}"
    );
    assert_same("mpm.manager.digest", body, result, &[]);
}

/// Test: this function IS the test.
#[tokio::test]
async fn rpc_manager_digest_rejects_a_malformed_scope() {
    let (state, _dir) = hermetic();

    let (status, _) = http(&state, "GET", "/api/v1/manager/digest?scope=nonsense", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let error = rpc_err(
        &rpc_router(&state),
        "mpm.manager.digest",
        json!({ "scope": "nonsense" }),
    )
    .await;
    assert_eq!(error["code"], json!(CODE_INVALID_PARAMS), "{error}");
}

/// Test: this function IS the test.
#[tokio::test]
async fn rpc_manager_digest_unknown_project_is_not_found() {
    let (state, _dir) = hermetic();

    let (status, _) = http(
        &state,
        "GET",
        "/api/v1/manager/digest?scope=project:nope",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let error = rpc_err(
        &rpc_router(&state),
        "mpm.manager.digest",
        json!({ "scope": "project:nope" }),
    )
    .await;
    assert_eq!(error["code"], json!(CODE_NOT_FOUND), "{error}");
}

/// Why the no-provider leg is the comparable one: with the seam forced off both
/// transports take the same 503 path, and no live model is called from a unit
/// suite.
/// Test: this function IS the test.
#[tokio::test]
async fn parity_manager_chat_agrees_across_transports() {
    let (state, _dir) = hermetic();
    without_a_provider(&state);
    let payload = json!({ "conversation_key": "k", "message": "hello" });

    let (status, body) = http(
        &state,
        "POST",
        "/api/v1/manager/chat",
        Some(payload.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"], json!("inference_unavailable"), "{body}");

    let error = rpc_err(&rpc_router(&state), "mpm.manager.chat", payload).await;
    assert_eq!(
        error["code"],
        json!(CODE_UNAVAILABLE),
        "the socket must report the same class the 503 did: {error}"
    );
}

/// Test: this function IS the test.
#[tokio::test]
async fn rpc_manager_chat_rejects_a_blank_conversation_key() {
    let (state, _dir) = hermetic();
    let payload = json!({ "conversation_key": "  ", "message": "hello" });

    let (status, body) = http(
        &state,
        "POST",
        "/api/v1/manager/chat",
        Some(payload.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body["error"],
        json!("invalid_request"),
        "the typed HTTP error body must be unchanged by the extraction: {body}"
    );

    let error = rpc_err(&rpc_router(&state), "mpm.manager.chat", payload).await;
    assert_eq!(error["code"], json!(CODE_INVALID_PARAMS), "{error}");
}

/// Test: this function IS the test.
#[tokio::test]
async fn parity_manager_route_task_agrees_across_transports() {
    let (state, _dir) = hermetic();
    let payload = json!({ "text": "fix the parser" });

    let (status, body) = http(
        &state,
        "POST",
        "/api/v1/manager/route-task",
        Some(payload.clone()),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "an empty registry is an advisory 200"
    );
    let result = rpc_ok(&rpc_router(&state), "mpm.manager.route_task", payload).await;
    assert_same("mpm.manager.route_task", body, result, &[]);
}

/// Test: this function IS the test.
#[tokio::test]
async fn rpc_manager_route_task_rejects_blank_text() {
    let (state, _dir) = hermetic();
    let payload = json!({ "text": "   " });

    let (status, body) = http(
        &state,
        "POST",
        "/api/v1/manager/route-task",
        Some(payload.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], json!("invalid_request"), "{body}");

    let error = rpc_err(&rpc_router(&state), "mpm.manager.route_task", payload).await;
    assert_eq!(error["code"], json!(CODE_INVALID_PARAMS), "{error}");
}

/// Why the propose leg and not the confirm leg: `confirm: false` is the arm that
/// must execute NOTHING, and proving both transports take it is what shows the
/// mutation gate did not move into a handler.
/// Test: this function IS the test.
#[tokio::test]
async fn parity_manager_act_propose_agrees_across_transports() {
    let (state, _dir) = hermetic();
    let payload = json!({
        "conversation_key": "k",
        "action": { "type": "summarize", "session": "s" },
    });

    let (status, body) = http(&state, "POST", "/api/v1/manager/act", Some(payload.clone())).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], json!("proposed"), "{body}");
    let result = rpc_ok(&rpc_router(&state), "mpm.manager.act", payload).await;
    assert_same("mpm.manager.act", body, result, &[]);
}

/// Test: this function IS the test.
#[tokio::test]
async fn rpc_manager_act_rejects_a_blank_conversation_key() {
    let (state, _dir) = hermetic();
    let payload = json!({
        "conversation_key": "  ",
        "action": { "type": "summarize", "session": "s" },
    });

    let (status, body) = http(&state, "POST", "/api/v1/manager/act", Some(payload.clone())).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], json!("invalid_request"), "{body}");

    let error = rpc_err(&rpc_router(&state), "mpm.manager.act", payload).await;
    assert_eq!(error["code"], json!(CODE_INVALID_PARAMS), "{error}");
}

// ── Pairing ──────────────────────────────────────────────────────────────────

/// Why `code` and `expires_in_seconds` are dropped: each call mints a fresh
/// six-character code, and the TTL is measured from the mint. The SHAPE is what
/// the two transports must agree on.
/// Test: this function IS the test.
#[tokio::test]
async fn parity_pair_request_agrees_across_transports() {
    let (state, _dir) = hermetic();

    let (status, body) = http(&state, "POST", "/pair/request", None).await;
    assert_eq!(status, StatusCode::OK);
    let result = rpc_ok(&rpc_router(&state), "mpm.pair.request", Value::Null).await;
    assert!(
        body["code"].as_str().is_some_and(|c| !c.is_empty()),
        "the HTTP call must mint a code: {body}"
    );
    assert!(
        result["code"].as_str().is_some_and(|c| !c.is_empty()),
        "the RPC call must mint one too: {result}"
    );
    assert_same("mpm.pair.request", body, result, &["code"]);
}

/// Why the code is minted first and used twice: the second confirm must fail —
/// a pairing code is one-time — so this proves the socket call really consumed
/// the same single-use code the HTTP call would have.
/// Test: this function IS the test.
#[tokio::test]
async fn parity_pair_confirm_agrees_across_transports() {
    let (state, _dir) = hermetic();

    let (_, minted) = http(&state, "POST", "/pair/request", None).await;
    let code = minted["code"].as_str().expect("a minted code").to_string();

    let result = rpc_ok(
        &rpc_router(&state),
        "mpm.pair.confirm",
        json!({ "code": code, "chat_id": 4242 }),
    )
    .await;
    assert_eq!(result["success"], json!(true), "{result}");
    assert_eq!(result["chat_id"], json!(4242), "{result}");

    // The store, not just the answer.
    let (_, status_body) = http(&state, "GET", "/pair/status", None).await;
    assert_eq!(status_body["paired"], json!(true), "{status_body}");
    assert_eq!(status_body["chat_id"], json!(4242), "{status_body}");

    // The same code a second time is refused, over HTTP, in the same body shape.
    let (status, second) = http(
        &state,
        "POST",
        "/pair/confirm",
        Some(json!({ "code": code, "chat_id": 9 })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the refusal is in the body, not the status"
    );
    assert_eq!(second["success"], json!(false), "{second}");
}

/// Why the refusal is asserted as a BODY and not an error frame: the route has
/// always reported a bad code inside a `200`, so a bot can tell "your code was
/// wrong" from "the daemon is unreachable". Turning it into an RPC error would
/// erase that distinction for a socket caller.
/// Test: this function IS the test.
#[tokio::test]
async fn rpc_pair_confirm_rejects_a_bad_code_in_the_body() {
    let (state, _dir) = hermetic();

    let (status, body) = http(
        &state,
        "POST",
        "/pair/confirm",
        Some(json!({ "code": "ZZZZZZ", "chat_id": 1 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let result = rpc_ok(
        &rpc_router(&state),
        "mpm.pair.confirm",
        json!({ "code": "ZZZZZZ", "chat_id": 1 }),
    )
    .await;
    assert_eq!(result["success"], json!(false), "{result}");
    assert_eq!(
        result["error"],
        json!("invalid or expired code"),
        "the reason must cross the socket verbatim: {result}"
    );
    assert_same("mpm.pair.confirm", body, result, &[]);
}

/// Test: this function IS the test.
#[tokio::test]
async fn parity_pair_status_agrees_across_transports() {
    let (state, _dir) = hermetic();

    let (status, body) = http(&state, "GET", "/pair/status", None).await;
    assert_eq!(status, StatusCode::OK);
    let result = rpc_ok(&rpc_router(&state), "mpm.pair.status", Value::Null).await;
    assert_eq!(body["paired"], json!(false), "{body}");
    assert_same("mpm.pair.status", body, result, &[]);
}

/// Why the pairing is established first: a reset against an unpaired daemon
/// would answer `{ reset: true }` either way, proving nothing.
/// Test: this function IS the test.
#[tokio::test]
async fn parity_pair_reset_agrees_across_transports() {
    let (state, _dir) = hermetic();
    let (_, minted) = http(&state, "POST", "/pair/request", None).await;
    let code = minted["code"].as_str().expect("a minted code").to_string();
    http(
        &state,
        "POST",
        "/pair/confirm",
        Some(json!({ "code": code, "chat_id": 7 })),
    )
    .await;

    let result = rpc_ok(&rpc_router(&state), "mpm.pair.reset", Value::Null).await;
    assert_eq!(result, json!({ "reset": true }), "{result}");

    let (_, after) = http(&state, "GET", "/pair/status", None).await;
    assert_eq!(
        after["paired"],
        json!(false),
        "the reset must clear the binding, not only answer: {after}"
    );

    // And the HTTP verb answers identically against an already-clear daemon.
    let (status, body) = http(&state, "POST", "/pair/reset", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_same("mpm.pair.reset", body, result, &[]);
}

// ── Delegation query ─────────────────────────────────────────────────────────

/// A dispatch payload the guard would send for an unisolated subagent.
fn dispatch_payload(cwd: &std::path::Path, tool_use_id: &str) -> Value {
    json!({
        "cwd": cwd.display().to_string(),
        "tool": "Task",
        "tool_use_id": tool_use_id,
        "input": { "subagent_type": "rust-engineer" }
    })
}

/// Why two different session ids and two different `tool_use_id`s: the route
/// answers AND claims in one critical section, so reusing either would let the
/// second call see the first's own claim and change the answer for reasons that
/// have nothing to do with the transport.
/// Test: this function IS the test.
#[tokio::test]
async fn parity_delegation_shared_tree_dispatch_agrees_across_transports() {
    let (state, dir) = hermetic();
    let a = uuid::Uuid::new_v4().to_string();
    let b = uuid::Uuid::new_v4().to_string();
    let tree_http = dir.path().join("tree-http");
    let tree_rpc = dir.path().join("tree-rpc");

    let (status, body) = http(
        &state,
        "POST",
        &format!("/api/v1/sessions/{a}/delegations/shared-tree-dispatch"),
        Some(json!({ "payload": dispatch_payload(&tree_http, "tu-http") })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let mut params = json!({ "id": b });
    params["payload"] = dispatch_payload(&tree_rpc, "tu-rpc");
    let result = rpc_ok(
        &rpc_router(&state),
        "mpm.delegation.shared_tree_dispatch",
        params,
    )
    .await;

    assert_eq!(
        body["claimed"],
        json!(true),
        "an empty tree must be claimed over HTTP: {body}"
    );
    assert_same("mpm.delegation.shared_tree_dispatch", body, result, &[]);
}

/// Test: this function IS the test.
#[tokio::test]
async fn parity_delegation_granted_worktree_agrees_across_transports() {
    let (state, dir) = hermetic();
    let a = uuid::Uuid::new_v4().to_string();
    let b = uuid::Uuid::new_v4().to_string();
    let tree_http = dir.path().join("granted-http");
    let tree_rpc = dir.path().join("granted-rpc");

    let mut http_payload = dispatch_payload(&tree_http, "tu-http");
    http_payload["input"]["isolation"] = json!("worktree");
    let mut rpc_payload = dispatch_payload(&tree_rpc, "tu-rpc");
    rpc_payload["input"]["isolation"] = json!("worktree");

    let (status, body) = http(
        &state,
        "POST",
        &format!("/api/v1/sessions/{a}/delegations/granted-worktree"),
        Some(json!({ "payload": http_payload })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let mut params = json!({ "id": b });
    params["payload"] = rpc_payload;
    let result = rpc_ok(
        &rpc_router(&state),
        "mpm.delegation.granted_worktree",
        params,
    )
    .await;
    assert_same("mpm.delegation.granted_worktree", body, result, &[]);
}

/// Why: a malformed session id is a `400` over HTTP, and the guard reads a
/// coded refusal differently from an empty answer — the two must not collapse.
/// Test: this function IS the test.
#[tokio::test]
async fn rpc_delegation_rejects_a_malformed_session_id() {
    let (state, dir) = hermetic();

    let (status, _) = http(
        &state,
        "POST",
        "/api/v1/sessions/not-a-uuid/delegations/shared-tree-dispatch",
        Some(json!({ "payload": dispatch_payload(dir.path(), "tu") })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let mut params = json!({ "id": "not-a-uuid" });
    params["payload"] = dispatch_payload(dir.path(), "tu");
    let error = rpc_err(
        &rpc_router(&state),
        "mpm.delegation.shared_tree_dispatch",
        params,
    )
    .await;
    assert_eq!(error["code"], json!(CODE_INVALID_PARAMS), "{error}");
}

// ── Error mapping, both ways ─────────────────────────────────────────────────

/// Why this pins the enum to the status table rather than testing one route: a
/// code derived from a status is only trustworthy while the derivation stays
/// total. A new [`DaemonError`] variant that forgets its row lands on the
/// catch-all, and the catch-all is what this asserts against.
/// Test: this function IS the test.
#[test]
fn rpc_error_codes_track_http_statuses_for_this_slice() {
    // The `DaemonError` classes this slice's routes actually raise.
    for (error, expected) in [
        (
            DaemonError::InvalidRequest("bad".into()),
            CODE_INVALID_PARAMS,
        ),
        (DaemonError::NotFound("gone".into()), CODE_NOT_FOUND),
        (
            DaemonError::ServiceUnavailable("no provider".into()),
            CODE_UNAVAILABLE,
        ),
        (
            DaemonError::UpstreamFailed("provider erred".into()),
            CODE_UPSTREAM_FAILED,
        ),
        (
            DaemonError::InvalidTransition {
                from: "proposed".into(),
                to: "complete".into(),
                allowed: vec!["in-progress".into()],
            },
            CODE_CONFLICT,
        ),
        (
            DaemonError::DeliverableNotFound { id: "d".into() },
            CODE_NOT_FOUND,
        ),
        (
            DaemonError::MilestoneNotFound { id: "m".into() },
            CODE_NOT_FOUND,
        ),
    ] {
        let message = error.to_string();
        let rpc: RpcError = error.into();
        assert_eq!(rpc.code, expected, "{message}");
        assert_eq!(
            rpc.message, message,
            "the message must cross verbatim so a parity assertion means something"
        );
    }
}
