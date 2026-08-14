//! Fail-open regression tests for issue #767 — `POST /indexes` must refuse a
//! root the allowlist does not approve.
//!
//! Why: PR #789 built `allowlist::check_path` and shipped it with ZERO
//! production call sites — `grep -rn check_path src/` matched only its own doc
//! comment and its own tests. A default-deny security control existed and
//! gated nothing, so every root reaching `POST /indexes` was registered exactly
//! as before the control was written. These tests exercise the handler, not the
//! helper, because a passing helper test is precisely what hid the gap.
//!
//! Every test here fails against the pre-fix commit: `create_index_handler`
//! answered `200 {"created": true}` for an un-allowlisted root.
//!
//! What: drives `create_index_handler` and `relocate_index_handler` directly
//! with `AllowlistPaths` pointed at tempdir fixtures, so the verdict never
//! depends on the developer's real config or `tm` registry.
//! Test: `cargo test -p trusty-search tests_allowlist_gate_767`.

use super::*;
use crate::allowlist::{AllowlistConfig, AllowlistEntry, AllowlistPaths};
use crate::core::embed::Embedder;
use crate::core::registry::IndexRegistry;
use axum::body::to_bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A real directory that passes the hard denylist.
///
/// Why: a `tempfile::tempdir()` under `/var/folders` is denied by prefix, so
/// the request would be refused before the allowlist gate is consulted and the
/// test would prove nothing about #767.
fn safe_root(name: &str) -> PathBuf {
    let base = dirs::home_dir()
        .expect("HOME required")
        .join(".trusty-search-gate767-tests");
    let dir = base.join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create test root");
    std::fs::canonicalize(&dir).expect("canonicalize test root")
}

/// Fixture allowlist paths over `dir`, approving `approved`.
fn fixture(dir: &Path, approved: &[&Path]) -> AllowlistPaths {
    let paths = AllowlistPaths::default()
        .with_allowlist(dir.join("allowlist.toml"))
        .with_project_paths(dir.join("projects.json"));
    let cfg = AllowlistConfig {
        entries: approved
            .iter()
            .map(|p| AllowlistEntry {
                path: p.to_path_buf(),
                name: None,
                exclude: Vec::new(),
                extensions: Vec::new(),
                skip_kg: false,
            })
            .collect(),
    };
    cfg.save_to(&paths.allowlist_file())
        .expect("write allowlist");
    paths
}

fn create_req(id: &str, root_path: PathBuf) -> super::router::CreateIndexRequest {
    super::router::CreateIndexRequest {
        id: id.to_string(),
        root_path,
        include_paths: None,
        exclude_globs: None,
        extensions: None,
        domain_terms: None,
        path_filter: None,
        include_docs: None,
        respect_gitignore: None,
        follow_links: None,
        lexical_only: None,
        skip_kg: None,
        skip_vector: None,
        defer_embed: None,
        extra_skip_dirs: None,
        data_file_max_bytes: None,
        allow_sensitive_path: false,
    }
}

async fn state_with(paths: AllowlistPaths) -> Arc<SearchAppState> {
    let state = SearchAppState::new(IndexRegistry::new()).with_allowlist_paths(paths);
    let embedder: Arc<dyn Embedder> = Arc::new(crate::core::embed::MockEmbedder::new(8));
    state.install_embedder(embedder).await;
    Arc::new(state)
}

async fn body_json(resp: axum::response::Response) -> (StatusCode, serde_json::Value) {
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), 65536).await.expect("body");
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

/// THE fail-open regression: an un-allowlisted root must be refused.
///
/// Why: before this fix the same call returned `200 {"created": true}` and the
/// index was registered. That is the exact behaviour #767's acceptance
/// criterion "a root is indexed only after being explicitly added to the
/// allowlist" forbids, and it was live from PR #789's merge until now.
/// What: empty allowlist, real existing non-sensitive directory, no project
/// registration → `403`, nothing in the registry.
#[tokio::test]
async fn create_index_refuses_unlisted_root() {
    let fx = tempfile::tempdir().expect("tempdir");
    let root = safe_root("unlisted");
    let state = state_with(fixture(fx.path(), &[])).await;

    let resp = create_index_handler(
        State(Arc::clone(&state)),
        Json(create_req("unlisted", root.clone())),
    )
    .await;
    let (status, json) = body_json(resp).await;

    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "an un-allowlisted root must be refused, got {status} {json}"
    );
    assert!(
        json["error"]
            .as_str()
            .unwrap_or_default()
            .contains("indexing refused"),
        "body must say why: {json}"
    );
    assert!(
        json["remedy"]
            .as_str()
            .unwrap_or_default()
            .contains("index add"),
        "refusal must be actionable: {json}"
    );
    assert!(
        state
            .registry
            .get(&crate::core::registry::IndexId::new("unlisted".to_string()))
            .is_none(),
        "refused root must not be registered"
    );
}

/// The same root, once approved, registers normally — the gate denies by
/// policy, not by breaking registration.
#[tokio::test]
async fn create_index_accepts_allowlisted_root() {
    let fx = tempfile::tempdir().expect("tempdir");
    let root = safe_root("approved");
    let state = state_with(fixture(fx.path(), &[&root])).await;

    let resp = create_index_handler(
        State(Arc::clone(&state)),
        Json(create_req("approved", root.clone())),
    )
    .await;
    let (status, json) = body_json(resp).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "approved root must register: {json}"
    );
    assert_eq!(json["created"], serde_json::Value::Bool(true), "{json}");
}

/// A root the `tm` project registry lists is approved with no `allowlist.toml`
/// entry — #767's "registered as a project" category.
#[tokio::test]
async fn create_index_accepts_registered_project_root() {
    let fx = tempfile::tempdir().expect("tempdir");
    let root = safe_root("project");
    let paths = fixture(fx.path(), &[]);
    std::fs::write(
        paths.project_paths_file(),
        serde_json::to_string(&[serde_json::json!({"alias":"p","path": &root})]).expect("json"),
    )
    .expect("write projects");
    let state = state_with(paths).await;

    let resp = create_index_handler(
        State(Arc::clone(&state)),
        Json(create_req("project", root.clone())),
    )
    .await;
    let (status, json) = body_json(resp).await;
    assert_eq!(status, StatusCode::OK, "project root must register: {json}");
}

/// A worktree provisioned under an approved root is approved by derivation —
/// #767's "explicitly provisioned locations" category.
#[tokio::test]
async fn create_index_accepts_provisioned_worktree() {
    let fx = tempfile::tempdir().expect("tempdir");
    let root = safe_root("wt-parent");
    let wt = root.join(".claude/worktrees/agent-767");
    std::fs::create_dir_all(&wt).expect("mkdir worktree");
    let state = state_with(fixture(fx.path(), &[&root])).await;

    let resp = create_index_handler(
        State(Arc::clone(&state)),
        Json(create_req("wt", wt.clone())),
    )
    .await;
    let (status, json) = body_json(resp).await;
    assert_eq!(status, StatusCode::OK, "worktree must register: {json}");
}

/// A SIBLING of an approved root is refused. Approving `~/Projects/foo` must
/// not approve `~/Projects/foo-scratch` — that is the unrelated-directory case
/// the 74-index incident was made of.
#[tokio::test]
async fn create_index_refuses_sibling_of_approved_root() {
    let fx = tempfile::tempdir().expect("tempdir");
    let root = safe_root("sibling-parent");
    let sibling = safe_root("sibling-parent-scratch");
    let state = state_with(fixture(fx.path(), &[&root])).await;

    let resp =
        create_index_handler(State(Arc::clone(&state)), Json(create_req("sib", sibling))).await;
    let (status, json) = body_json(resp).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{json}");
}

/// A sub-root INSIDE an approved root registers — `trusty-search.yaml`
/// multi-index repos declare exactly this shape, and the sub-root exposes
/// strictly less than the approved root already does.
#[tokio::test]
async fn create_index_accepts_subdirectory_of_approved_root() {
    let fx = tempfile::tempdir().expect("tempdir");
    let root = safe_root("subdir-parent");
    let sub = root.join("services/api");
    std::fs::create_dir_all(&sub).expect("mkdir");
    let state = state_with(fixture(fx.path(), &[&root])).await;

    let resp = create_index_handler(State(Arc::clone(&state)), Json(create_req("sub", sub))).await;
    let (status, json) = body_json(resp).await;
    assert_eq!(status, StatusCode::OK, "{json}");
}

/// The hard denylist still wins over an explicit allowlist entry, and still
/// answers `400` rather than the gate's `403` — the denylist is a different
/// refusal from "not approved" and keeps its own contract.
#[tokio::test]
async fn denylist_still_wins_over_an_allowlist_entry() {
    let ssh = dirs::home_dir().expect("home").join(".ssh");
    if !ssh.is_dir() {
        return;
    }
    let fx = tempfile::tempdir().expect("tempdir");
    let state = state_with(fixture(fx.path(), &[&ssh])).await;

    let resp = create_index_handler(
        State(Arc::clone(&state)),
        Json(create_req("ssh", ssh.clone())),
    )
    .await;
    let (status, json) = body_json(resp).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "denylist must beat the allowlist: {json}"
    );
}

/// `allow_sensitive_path` relaxes the temp-dir PREFIX check only. It must not
/// become a hole in the opt-in gate: an un-approved temp root is still refused.
#[tokio::test]
async fn allow_sensitive_path_does_not_bypass_the_allowlist() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = std::fs::canonicalize(tmp.path()).expect("canonicalize");
    let fx = tempfile::tempdir().expect("tempdir");
    let state = state_with(fixture(fx.path(), &[])).await;

    let mut req = create_req("sensitive-optin", root);
    req.allow_sensitive_path = true;
    let resp = create_index_handler(State(Arc::clone(&state)), Json(req)).await;
    let (status, json) = body_json(resp).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "opting past the temp-dir prefix must not opt past the allowlist: {json}"
    );
}

/// Relocating an index onto an un-approved root is refused — relocate is index
/// creation at a new root and must not be a side door around the gate.
#[tokio::test]
async fn relocate_refuses_unlisted_root() {
    let fx = tempfile::tempdir().expect("tempdir");
    let approved = safe_root("reloc-approved");
    let elsewhere = safe_root("reloc-elsewhere");
    let state = state_with(fixture(fx.path(), &[&approved])).await;

    let resp = create_index_handler(
        State(Arc::clone(&state)),
        Json(create_req("reloc", approved.clone())),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK, "setup: create must succeed");

    let resp = super::indexes_relocate::relocate_index_handler(
        State(Arc::clone(&state)),
        axum::extract::Path("reloc".to_string()),
        Json(super::indexes_relocate::RelocateIndexRequest {
            root_path: elsewhere.clone(),
        }),
    )
    .await;
    let (status, json) = body_json(resp).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "relocate onto an unapproved root must be refused: {json}"
    );
}
