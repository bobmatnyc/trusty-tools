//! #10739: `include_paths` in `search.index.config.get` / `config.set`.
//!
//! Why: SPEC-OKF-01 §5.3.1 rule 4 needs the reply to prove which paths a
//! binding admits, and an OKG binding created with `include_paths: ["notes"]`
//! could never be widened, because `config.set` copied the list through.
//! What: real create / PATCH / reindex handlers over an OKF-shaped bundle.
//! The PATCH requests are decoded from JSON, as the socket decodes them.
//! Test: this module. Run with `cargo test -p trusty-search include_paths_10739`.

use super::index_config::{index_config_report, patch_index_config_report};
use super::profile_10532_tests::{chunks_of, reindex};
use super::tests_8499::{clean_repo, create_req, mock_state};
use super::tests_components::IsolatedDataDir;
use super::*;
use crate::core::registry::IndexId;
use crate::service::index_admission::Admission;
use crate::service::write_admission::admits_pushed;
use std::path::Path;
use std::sync::Arc;

pub(super) const REGISTRY: &str = "_sources/registry.toml";
pub(super) const STATE: &str = "_state/x.md";
pub(super) const INDEX: &str = "index.md";
pub(super) const DRIVE: &str = "drive/a/b.md";
const NOTE: &str = "notes/n.md";

/// The bundle-root exclusion binding SPEC-OKF-01 §5.3.1 rule 1 states.
pub(super) fn okf_excludes() -> Vec<String> {
    vec!["_sources/**".to_string(), "_state/**".to_string()]
}

/// An OKF-shaped bundle: the two private trees, a root concept, a nested
/// concept and a note.
pub(super) fn bundle() -> (tempfile::TempDir, std::path::PathBuf) {
    let (dir, root) = clean_repo("okf-10739-", None);
    for (rel, body) in [
        (REGISTRY, "[roots]\ndrive = \"gmail query\"\n"),
        (STATE, "# conversion log\nprivate state\n"),
        (INDEX, "# Index\nroot concept about widgets\n"),
        (DRIVE, "# B\nnested concept about gadgets\n"),
        (NOTE, "# Note\na note about sprockets\n"),
    ] {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
        std::fs::write(path, body).expect("write fixture");
    }
    (dir, root)
}

/// Create `id` at `root` through the real handler.
pub(super) async fn create(
    state: &Arc<SearchAppState>,
    id: &str,
    root: &Path,
    include_paths: Option<Vec<String>>,
    exclude_globs: Vec<String>,
) {
    let mut req = create_req(id, root.to_path_buf());
    req.include_paths = include_paths;
    req.exclude_globs = Some(exclude_globs);
    let (status, body) = super::test_support::create_answer(state, req).await;
    assert_eq!(status, axum::http::StatusCode::OK, "{body}");
}

fn admission(state: &Arc<SearchAppState>, id: &str, rel: &str) -> Admission {
    let handle = state.registry.get(&IndexId::new(id)).expect("registered");
    admits_pushed(&handle, rel, 0)
}

fn config(state: &Arc<SearchAppState>, id: &str) -> serde_json::Value {
    serde_json::to_value(index_config_report(state, id).expect("config.get")).expect("json")
}

async fn patch(
    state: &Arc<SearchAppState>,
    id: &str,
    body: serde_json::Value,
) -> serde_json::Value {
    let req = serde_json::from_value(body).expect("PATCH body decodes");
    patch_index_config_report(state, id, req)
        .await
        .expect("config.set")
}

/// config.get reports `include_paths` (root-relative) and `path_filter`
/// beside the exclusion filters, so the reply alone is the §5.3.1 proof.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn config_get_reports_include_paths_and_path_filter() {
    let _data = IsolatedDataDir::new();
    let state = mock_state().await;
    let (_dir, root) = bundle();
    let mut req = create_req("inc-get", root.clone());
    req.include_paths = Some(vec!["notes".into(), "drive".into()]);
    req.exclude_globs = Some(okf_excludes());
    req.path_filter = Some(vec!["dr*".into()]);
    let (status, body) = super::test_support::create_answer(&state, req).await;
    assert_eq!(status, axum::http::StatusCode::OK, "{body}");

    let cfg = config(&state, "inc-get");
    assert_eq!(
        cfg["include_paths"],
        serde_json::json!(["notes", "drive"]),
        "{cfg}"
    );
    assert_eq!(cfg["path_filter"], serde_json::json!(["dr*"]), "{cfg}");
    assert_eq!(cfg["exclude_globs"], serde_json::json!(okf_excludes()));
    assert_eq!(cfg["invalid_exclude_globs"], serde_json::json!([]));

    // An index with no allowlist reports an empty list, not a missing field.
    let (_other, other_root) = bundle();
    create(&state, "inc-get-all", &other_root, None, okf_excludes()).await;
    let cfg = config(&state, "inc-get-all");
    assert_eq!(cfg["include_paths"], serde_json::json!([]), "{cfg}");
    assert_eq!(cfg["path_filter"], serde_json::json!([]), "{cfg}");
}

/// config.set with `include_paths: []` clears a notes-only allowlist: the
/// reply, the stored row and the live admission all widen, `_sources/` stays
/// refused, and the next reindex indexes the newly admitted concepts.
/// Test: this test. On a daemon that copies `include_paths` through, the
/// first post-PATCH assertion fails.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn config_set_clears_include_paths_and_refused_paths_become_admissible() {
    let _data = IsolatedDataDir::new();
    let state = mock_state().await;
    let (_dir, root) = bundle();
    let id = "inc-clear";
    create(
        &state,
        id,
        &root,
        Some(vec!["notes".into()]),
        okf_excludes(),
    )
    .await;
    assert_eq!(admission(&state, id, INDEX), Admission::Excluded);
    assert_eq!(admission(&state, id, DRIVE), Admission::Excluded);

    let reply = patch(&state, id, serde_json::json!({ "include_paths": [] })).await;
    // Live admission first, so a daemon that ignores the field fails on behaviour.
    assert_eq!(admission(&state, id, INDEX), Admission::Included);
    assert_eq!(admission(&state, id, DRIVE), Admission::Included);
    assert_eq!(admission(&state, id, NOTE), Admission::Included);
    assert_eq!(admission(&state, id, REGISTRY), Admission::Excluded);
    assert_eq!(admission(&state, id, STATE), Admission::Excluded);

    assert_eq!(
        reply["config"]["include_paths"],
        serde_json::json!([]),
        "{reply}"
    );
    assert_eq!(config(&state, id)["include_paths"], serde_json::json!([]));
    let row = crate::service::persistence::load_index_registry()
        .expect("registry")
        .into_iter()
        .find(|e| e.id == id)
        .expect("row");
    assert!(row.include_paths.is_empty(), "{:?}", row.include_paths);

    // Newly admitted files are indexed by the next reindex, not the PATCH.
    assert_eq!(chunks_of(&state, id, INDEX).await, 0);
    reindex(&state, id).await;
    assert!(chunks_of(&state, id, INDEX).await > 0);
    assert!(chunks_of(&state, id, DRIVE).await > 0);
    assert_eq!(chunks_of(&state, id, REGISTRY).await, 0);
    assert_eq!(chunks_of(&state, id, STATE).await, 0);
}

/// Narrowing `include_paths` purges the files the new list refuses in place,
/// as an `exclude_globs` change does (#10532), and runs no reindex.
/// Test: this test. Without the purge, `index.md` keeps its chunks.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn setting_include_paths_purges_newly_refused_files() {
    let _data = IsolatedDataDir::new();
    let state = mock_state().await;
    let (_dir, root) = bundle();
    let id = "inc-narrow";
    create(&state, id, &root, None, okf_excludes()).await;
    reindex(&state, id).await;
    for kept in [INDEX, DRIVE, NOTE] {
        assert!(chunks_of(&state, id, kept).await > 0, "{kept}");
    }
    let handle = state.registry.get(&IndexId::new(id)).expect("handle");
    let walked_before = handle
        .walk_diagnostics
        .read()
        .await
        .last_walk_started_at
        .clone();

    let reply = patch(
        &state,
        id,
        serde_json::json!({ "include_paths": ["notes"] }),
    )
    .await;
    // The corpus first, so a daemon that skips the purge fails on behaviour.
    for gone in [INDEX, DRIVE, "src/lib.rs"] {
        assert_eq!(chunks_of(&state, id, gone).await, 0, "{gone}");
    }
    assert!(chunks_of(&state, id, NOTE).await > 0);
    assert_eq!(
        reply["config"]["include_paths"],
        serde_json::json!(["notes"]),
        "{reply}"
    );
    let purge = &reply["include_paths_purge"];
    assert_eq!(purge["rebuilt"], false, "{reply}");
    // index.md, drive/a/b.md and the fixture's src/lib.rs.
    assert_eq!(purge["files_removed"], 3, "{reply}");
    assert!(reply.get("profile_purge").is_none(), "{reply}");
    assert_eq!(admission(&state, id, INDEX), Admission::Excluded);
    let handle = state.registry.get(&IndexId::new(id)).expect("handle");
    assert_eq!(
        handle.walk_diagnostics.read().await.last_walk_started_at,
        walked_before,
        "the purge must not rebuild"
    );
}

/// An include path that leaves the root is refused and changes nothing.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn config_set_refuses_an_include_path_outside_the_root() {
    let _data = IsolatedDataDir::new();
    let state = mock_state().await;
    let (_dir, root) = bundle();
    let id = "inc-escape";
    create(
        &state,
        id,
        &root,
        Some(vec!["notes".into()]),
        okf_excludes(),
    )
    .await;
    for bad in [
        serde_json::json!(["../elsewhere"]),
        serde_json::json!(["/etc"]),
    ] {
        let req = serde_json::from_value(serde_json::json!({ "include_paths": bad })).unwrap();
        let (status, body) = patch_index_config_report(&state, id, req)
            .await
            .expect_err("refused");
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"], "invalid_include_path", "{body}");
    }
    assert_eq!(
        config(&state, id)["include_paths"],
        serde_json::json!(["notes"])
    );
}

/// A directory outside any index root holding one marked Markdown file.
#[cfg(unix)]
fn outside_tree() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("outside dir");
    let real = dir.path().canonicalize().expect("canonical outside dir");
    std::fs::write(real.join("outside.md"), format!("# Outside\n{OUTSIDE}\n")).expect("write");
    (dir, real)
}

/// Content that only the out-of-root file carries.
#[cfg(unix)]
const OUTSIDE: &str = "outsidemarker10739 private content";

/// #10739: an include path that is a symlink to a directory outside the
/// root, or a dangling symlink, is refused at PATCH time and changes nothing.
/// Test: this test. A lexical-only check accepts both links.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn config_set_refuses_an_include_path_symlinked_outside_the_root() {
    let _data = IsolatedDataDir::new();
    let state = mock_state().await;
    let (_dir, root) = bundle();
    let (_outside, outside) = outside_tree();
    std::os::unix::fs::symlink(&outside, root.join("linked")).expect("symlink");
    std::os::unix::fs::symlink(root.join("gone"), root.join("dangling")).expect("symlink");
    let id = "inc-symlink";
    create(
        &state,
        id,
        &root,
        Some(vec!["notes".into()]),
        okf_excludes(),
    )
    .await;
    for bad in ["linked", "dangling"] {
        let req =
            serde_json::from_value(serde_json::json!({ "include_paths": ["notes", bad] })).unwrap();
        let (status, body) = patch_index_config_report(&state, id, req)
            .await
            .expect_err("refused");
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "{bad}: {body}");
        assert_eq!(body["error"], "invalid_include_path", "{bad}: {body}");
    }
    assert_eq!(
        config(&state, id)["include_paths"],
        serde_json::json!(["notes"])
    );
}

/// #10739: an include path absent at PATCH time that later becomes a symlink
/// to outside the root is not walked, so the reindex indexes no file from it.
/// Test: this test. Without the walk guard, `outside.md` is indexed.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn a_later_out_of_root_symlink_include_path_indexes_nothing_outside() {
    let _data = IsolatedDataDir::new();
    let state = mock_state().await;
    let (_dir, root) = bundle();
    let (_outside, outside) = outside_tree();
    let id = "inc-late-link";
    create(&state, id, &root, None, okf_excludes()).await;
    // Absent at PATCH time, so the PATCH admits it.
    patch(
        &state,
        id,
        serde_json::json!({ "include_paths": ["notes", "later"] }),
    )
    .await;
    std::os::unix::fs::symlink(&outside, root.join("later")).expect("symlink");

    reindex(&state, id).await;
    // The reindex ran: the in-root include path is indexed.
    assert!(chunks_of(&state, id, NOTE).await > 0);
    let handle = state.registry.get(&IndexId::new(id)).expect("handle");
    let leaked: Vec<String> = handle
        .indexer
        .read()
        .await
        .all_chunks()
        .await
        .into_iter()
        .filter(|c| c.content.contains("outsidemarker10739"))
        .map(|c| c.file.clone())
        .collect();
    assert!(leaked.is_empty(), "out-of-root files indexed: {leaked:?}");
}
