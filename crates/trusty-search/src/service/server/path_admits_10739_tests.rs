//! #10739: `search.index.path.admits` against the reindex walk.
//!
//! Why: the query is only useful if it gives the walk's answer, so each case
//! compares the two on an OKF-shaped bundle.
//! What: real create handler, `index_admission::walk` and [`path_admits_report`].
//! Test: this module. Run with `cargo test -p trusty-search path_admits_10739`.

use super::*;
use crate::service::server::include_paths_10739_tests::{
    bundle, create, okf_excludes, DRIVE, INDEX, REGISTRY, STATE,
};
use crate::service::server::tests_8499::mock_state;
use crate::service::server::tests_components::IsolatedDataDir;
use std::collections::BTreeSet;

const PATHS: [&str; 4] = [REGISTRY, STATE, INDEX, DRIVE];

/// The set of `PATHS` the reindex walk of `id` yields.
fn walked(state: &Arc<SearchAppState>, id: &str, root: &Path) -> BTreeSet<&'static str> {
    let handle = state.registry.get(&IndexId::new(id)).expect("registered");
    let files: BTreeSet<PathBuf> = index_admission::walk(&handle)
        .files
        .into_iter()
        .map(|f| f.canonicalize().unwrap_or(f))
        .collect();
    PATHS
        .into_iter()
        .filter(|rel| files.contains(&root.join(rel).canonicalize().expect("fixture exists")))
        .collect()
}

fn admitted(state: &Arc<SearchAppState>, id: &str) -> BTreeSet<&'static str> {
    PATHS
        .into_iter()
        .filter(|rel| path_admits_report(state, id, rel).expect("answer").admitted)
        .collect()
}

/// For the §5.3.1 exclusion binding and for an allowlist binding, the query
/// admits exactly the paths the reindex walk yields.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn path_admits_agrees_with_the_reindex_walk() {
    let _data = IsolatedDataDir::new();
    let state = mock_state().await;
    let (_a, root_a) = bundle();
    create(&state, "adm-exclude", &root_a, None, okf_excludes()).await;
    let (_b, root_b) = bundle();
    create(
        &state,
        "adm-allow",
        &root_b,
        Some(vec!["drive".into()]),
        vec!["nothing/**".into()],
    )
    .await;

    for (id, root, expected) in [
        ("adm-exclude", &root_a, BTreeSet::from([INDEX, DRIVE])),
        ("adm-allow", &root_b, BTreeSet::from([DRIVE])),
    ] {
        assert_eq!(walked(&state, id, root), expected, "{id}: walk");
        assert_eq!(admitted(&state, id), expected, "{id}: path.admits");
    }
}

/// The reason names the rule that refused the path.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn path_admits_names_the_refusing_rule() {
    let _data = IsolatedDataDir::new();
    let state = mock_state().await;
    let (_a, root_a) = bundle();
    create(&state, "why-exclude", &root_a, None, okf_excludes()).await;
    let (_b, root_b) = bundle();
    create(
        &state,
        "why-allow",
        &root_b,
        Some(vec!["drive".into()]),
        vec!["nothing/**".into()],
    )
    .await;
    let (_c, root_c) = bundle();
    let mut req = crate::service::server::tests_8499::create_req("why-ext", root_c);
    req.extensions = Some(vec!["md".into()]);
    req.exclude_globs = Some(vec!["nothing/**".into()]);
    let (status, body) = crate::service::server::test_support::create_answer(&state, req).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    for (id, path, reason) in [
        ("why-exclude", INDEX, "admitted"),
        ("why-exclude", REGISTRY, "exclude_globs"),
        ("why-exclude", STATE, "exclude_globs"),
        ("why-exclude", "../outside.md", "path_escapes_root"),
        ("why-exclude", "node_modules/pkg/x.js", "walker"),
        ("why-allow", INDEX, "outside_include_paths"),
        ("why-allow", DRIVE, "admitted"),
        ("why-ext", REGISTRY, "filters"),
    ] {
        let answer = path_admits_report(&state, id, path).expect("answer");
        assert_eq!(answer.reason, reason, "{id} {path}: {answer:?}");
        assert_eq!(answer.admitted, reason == "admitted", "{id} {path}");
    }
}

/// An unknown index is `404`.
/// Test: this test.
#[tokio::test]
#[serial_test::parallel]
async fn path_admits_refuses_an_unknown_index() {
    let state = Arc::new(SearchAppState::new(
        crate::core::registry::IndexRegistry::new(),
    ));
    let (status, _) = path_admits_report(&state, "ghost", INDEX).expect_err("unknown");
    assert_eq!(status, StatusCode::NOT_FOUND);
}
