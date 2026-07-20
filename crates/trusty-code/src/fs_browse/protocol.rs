//! `fs.*` JSON-RPC method handlers — the daemon's directory inspection surface.
//!
//! Why: screen 7a's folder picker needs to browse the local disk, and the UI is
//! a thin client that never touches the filesystem itself (see the parent
//! module's docs for the full rationale, including why no path guard belongs
//! here). Reaching it over `/rpc` alongside `session.*`/`task.*` — rather than
//! as a new REST resource — keeps the API exactly what it is today: one
//! JSON-RPC endpoint, three HTTP routes.
//! What: [`register`] wires `fs.list_dir` onto a [`Router`]. The handler parses
//! typed params, forwards to [`super::list_dir`], and maps
//! [`super::ListDirError`] onto the vision spec's §13.2 domain taxonomy via
//! [`to_rpc_error`] — reusing the codes already in the table rather than
//! inventing `fs`-specific ones.
//! Test: `protocol::tests::*`.

use serde::Deserialize;
use serde_json::{Value, json};

use crate::jsonrpc::{ConnectionContext, Router, RpcError};

use super::ListDirError;

/// `params` shape for `fs.list_dir`.
///
/// Why: both fields are optional so the picker's very first call — the
/// projectless cold start, where the client has no path to offer — is simply
/// `fs.list_dir({})`.
/// What: `path` defaults to `~` (the natural place to start browsing for
/// projects); `include_hidden` defaults to `false`.
/// Test: `tests::omitted_path_defaults_to_home`.
#[derive(Deserialize)]
struct ListDirParams {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    include_hidden: Option<bool>,
}

/// Register every `fs.*` method onto `router`.
///
/// Why: mirrors `session::protocol::register` / `task::protocol::register` —
/// the one place listing this namespace's full surface, so `serve::build_router`
/// stays a one-line call per group.
/// What: registers `"fs.list_dir"` and `"fs.list_projects"` (issue #3365's
/// project-picker roster). Unlike the `session.*`/`task.*` groups this takes
/// no shared state: both are pure functions of the filesystem at call time,
/// with no registry to close over.
/// Test: `tests::register_wires_fs_list_dir`,
/// `tests::register_wires_fs_list_projects`.
pub fn register(router: &mut Router) {
    router.register("fs.list_dir", list_dir);
    router.register("fs.list_projects", list_projects);
}

/// Map a typed [`ListDirError`] onto the vision spec's §13.2 error taxonomy.
///
/// Why: the picker must distinguish causes without substring-matching a
/// message; every variant gets a distinct code carrying a stable
/// `data.error_type`. These are the spec's EXISTING codes — a missing path is
/// the same `not_found` (-32002) an unknown agent is, and an OS refusal is the
/// same `permission_denied` (-32001) an RBAC refusal is — so no `fs`-specific
/// code enters the table.
/// What: `NotFound` → -32002 `not_found`; `PermissionDenied` → -32001
/// `permission_denied`; `NotADirectory`/`NoHome` → -32003 `invalid_argument`
/// (the caller named a path that is semantically wrong for this method);
/// `Io` → -32603 `internal` (an OS fault is not the caller's error).
/// Test: `tests::nonexistent_path_maps_to_not_found`,
/// `tests::file_path_maps_to_invalid_argument`,
/// `tests::errors_are_distinguishable_by_code`.
fn to_rpc_error(err: ListDirError) -> RpcError {
    let message = err.to_string();
    match err {
        ListDirError::NotFound(_) => RpcError::not_found(message),
        ListDirError::PermissionDenied(_) => RpcError::permission_denied(message),
        ListDirError::NotADirectory(_) | ListDirError::NoHome(_) => {
            RpcError::invalid_argument(message)
        }
        ListDirError::Io { .. } => RpcError::internal(message),
    }
}

/// `fs.list_dir(path?, include_hidden?) -> DirListing` (UI Phase-1, screen 7a).
///
/// Why: the daemon-side folder picker — the only way a thin UI client can browse
/// the local disk to choose a project root. One call answers both of 7a's
/// questions: what to draw (breadcrumb + rows + `git` badges) and what each row
/// would bind to (a git repo, or an equally-supported non-git directory).
/// What: `path` defaults to `~`; `~` is expanded and the path canonicalized
/// server-side, so a client navigates up by passing the previous response's
/// `parent`. Returns [`super::DirListing`] serialized whole — `{path,
/// display_path, parent, entries[]}`. Error codes per [`to_rpc_error`]. Note a
/// non-git directory is a normal success with `is_git_repo: false`, NOT an
/// error (#2728).
/// Test: `tests::lists_a_directory`, `tests::omitted_path_defaults_to_home`,
/// `tests::response_shape_supports_7a_rendering`.
async fn list_dir(params: Value, _ctx: ConnectionContext) -> Result<Value, RpcError> {
    // `params` is absent for a no-argument call; treat that as `{}` so
    // `fs.list_dir()` and `fs.list_dir({})` behave identically.
    let params = if params.is_null() { json!({}) } else { params };
    let p: ListDirParams = serde_json::from_value(params)
        .map_err(|e| RpcError::invalid_params(format!("fs.list_dir: {e}")))?;

    let path = p.path.as_deref().unwrap_or("~");
    let listing = super::list_dir(path, p.include_hidden.unwrap_or(false)).map_err(to_rpc_error)?;

    serde_json::to_value(listing)
        .map_err(|e| RpcError::internal(format!("fs.list_dir: serializing listing: {e}")))
}

/// `fs.list_projects() -> ProjectRoster` — the known-project roster for the
/// GUI's workstream-first project-picker modal (issue #3365).
///
/// Why: the modal's supported roster data source (see
/// `crate::fs_browse::roster` module docs for the discovery heuristic and
/// why this exists alongside, not instead of, `fs.list_dir`).
/// What: no params — `params` is ignored entirely (a caller passing `{}` or
/// omitting params behaves identically, matching `fs.list_dir`'s tolerance
/// for a no-argument call). Never returns a caller-facing error: an
/// unreadable/missing scan root degrades to an empty roster (see
/// `roster::list_projects`'s best-effort docs), so the only failure mode
/// here is JSON serialization, which cannot fail for this struct shape in
/// practice — kept as a `Result` only for symmetry with every other
/// registered method and to surface a genuine bug rather than `unwrap`.
/// Test: `tests::list_projects_returns_entries_array`,
/// `tests::register_wires_fs_list_projects`.
async fn list_projects(_params: Value, _ctx: ConnectionContext) -> Result<Value, RpcError> {
    serde_json::to_value(super::roster::list_projects())
        .map_err(|e| RpcError::internal(format!("fs.list_projects: serializing roster: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    fn test_ctx() -> ConnectionContext {
        let (tx, _rx) = mpsc::unbounded_channel();
        ConnectionContext::new(tx)
    }

    /// `fs.list_dir` on a real directory returns its entries.
    #[tokio::test]
    async fn lists_a_directory() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(tmp.path().join("alpha")).expect("mkdir");

        let result = list_dir(
            json!({"path": tmp.path().display().to_string()}),
            test_ctx(),
        )
        .await
        .expect("list_dir must succeed");

        assert_eq!(result["entries"][0]["name"], "alpha");
        assert_eq!(result["entries"][0]["is_dir"], true);
    }

    /// An omitted `path` must default to the user's home rather than erroring —
    /// the projectless cold-start call is `fs.list_dir({})`.
    #[tokio::test]
    async fn omitted_path_defaults_to_home() {
        let Some(home) = dirs::home_dir() else {
            return; // no home on this box; nothing to assert
        };
        let result = list_dir(json!({}), test_ctx())
            .await
            .expect("default list_dir must succeed");
        let expected = std::fs::canonicalize(&home).unwrap_or(home);
        assert_eq!(result["path"], expected.display().to_string());
        assert_eq!(result["display_path"], "~");
    }

    /// Absent `params` (a no-argument call) must behave like `{}`.
    #[tokio::test]
    async fn null_params_are_treated_as_empty_object() {
        if dirs::home_dir().is_none() {
            return;
        }
        let result = list_dir(Value::Null, test_ctx())
            .await
            .expect("null params must succeed");
        assert_eq!(result["display_path"], "~");
    }

    /// The response must carry everything screen 7a renders: a breadcrumb
    /// caption, an up-target, and per-row name/path/is_dir/git-badge state.
    #[tokio::test]
    async fn response_shape_supports_7a_rendering() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("acme-api");
        std::fs::create_dir(&repo).expect("mkdir");
        std::fs::create_dir(repo.join(".git")).expect("mkdir .git");
        std::fs::create_dir(tmp.path().join("scratch")).expect("mkdir");

        let result = list_dir(
            json!({"path": tmp.path().display().to_string()}),
            test_ctx(),
        )
        .await
        .expect("list_dir must succeed");

        // Breadcrumb + up-navigation.
        assert!(result["display_path"].is_string());
        assert!(result["parent"].is_string());

        // `acme-api  git` and `scratch  —` — both rows present, badge only on
        // the repo, and each row carries the path the client binds/navigates to.
        let entries = result["entries"].as_array().expect("entries array");
        let acme = entries
            .iter()
            .find(|e| e["name"] == "acme-api")
            .expect("acme-api row");
        let scratch = entries
            .iter()
            .find(|e| e["name"] == "scratch")
            .expect("scratch row");
        assert_eq!(acme["is_git_repo"], true);
        assert_eq!(scratch["is_git_repo"], false);
        assert!(acme["path"].is_string());
        assert_eq!(acme["is_dir"], true);
    }

    /// A nonexistent path must map to `-32002 not_found`.
    #[tokio::test]
    async fn nonexistent_path_maps_to_not_found() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let missing = tmp.path().join("no-such-dir");

        let err = list_dir(json!({"path": missing.display().to_string()}), test_ctx())
            .await
            .expect_err("must fail");
        assert_eq!(err.code, -32002);
        assert_eq!(err.data, Some(json!({"error_type": "not_found"})));
    }

    /// A path that exists but is a FILE must map to `-32003 invalid_argument`,
    /// distinct from the not-found code.
    #[tokio::test]
    async fn file_path_maps_to_invalid_argument() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let file = tmp.path().join("a.txt");
        std::fs::write(&file, "x").expect("write");

        let err = list_dir(json!({"path": file.display().to_string()}), test_ctx())
            .await
            .expect_err("must fail");
        assert_eq!(err.code, -32003);
        assert_eq!(err.data, Some(json!({"error_type": "invalid_argument"})));
    }

    /// The three caller-actionable failures must be tellable apart by code —
    /// the whole point of the typed error (a client must not substring-match).
    #[test]
    fn errors_are_distinguishable_by_code() {
        let not_found = to_rpc_error(ListDirError::NotFound("/x".into()));
        let not_dir = to_rpc_error(ListDirError::NotADirectory("/x".into()));
        let denied = to_rpc_error(ListDirError::PermissionDenied("/x".into()));
        let io = to_rpc_error(ListDirError::Io {
            path: "/x".into(),
            source: std::io::Error::other("boom"),
        });

        assert_eq!(not_found.code, -32002);
        assert_eq!(not_dir.code, -32003);
        assert_eq!(denied.code, -32001);
        assert_eq!(io.code, -32603);

        let codes = [not_found.code, not_dir.code, denied.code, io.code];
        let mut unique = codes.to_vec();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), codes.len(), "every cause needs its own code");
    }

    /// Malformed params (wrong type) must be rejected as invalid params rather
    /// than silently defaulting.
    #[tokio::test]
    async fn wrong_typed_params_are_rejected() {
        let err = list_dir(json!({"path": 42}), test_ctx())
            .await
            .expect_err("must fail");
        assert_eq!(err.code, -32602);
    }

    /// `register` must wire `fs.list_dir` so the router recognises it.
    #[tokio::test]
    async fn register_wires_fs_list_dir() {
        use trusty_common::mcp::Request;

        let mut router = Router::new();
        register(&mut router);

        let tmp = tempfile::tempdir().expect("tempdir");
        let req = Request {
            jsonrpc: Some("2.0".to_string()),
            id: Some(json!(1)),
            method: "fs.list_dir".to_string(),
            params: Some(json!({"path": tmp.path().display().to_string()})),
        };
        let resp = router.dispatch(req, &test_ctx()).await;
        assert!(
            resp.error.is_none(),
            "fs.list_dir must be registered, got {:?}",
            resp.error
        );
    }

    /// `fs.list_projects` must never error and must return an `entries`
    /// array — the response shape the picker modal dereferences. This runs
    /// against the REAL developer/CI-runner home directory (the handler has
    /// no injectable seam, unlike `roster::list_projects_in`, which carries
    /// the real scan-behaviour coverage), so this only pins the wire shape,
    /// not any particular content.
    #[tokio::test]
    async fn list_projects_returns_entries_array() {
        let result = list_projects(Value::Null, test_ctx())
            .await
            .expect("fs.list_projects must not error");
        assert!(result["entries"].is_array());
    }

    /// `register` must wire `fs.list_projects` so the router recognises it.
    #[tokio::test]
    async fn register_wires_fs_list_projects() {
        use trusty_common::mcp::Request;

        let mut router = Router::new();
        register(&mut router);

        let req = Request {
            jsonrpc: Some("2.0".to_string()),
            id: Some(json!(1)),
            method: "fs.list_projects".to_string(),
            params: None,
        };
        let resp = router.dispatch(req, &test_ctx()).await;
        assert!(
            resp.error.is_none(),
            "fs.list_projects must be registered, got {:?}",
            resp.error
        );
    }
}
