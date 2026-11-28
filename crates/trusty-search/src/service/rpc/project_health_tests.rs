//! `search.project.health` over the socket router (#10535).
//!
//! Why: the counts are tm's hygiene evidence, so each definition is pinned on
//! a real `indexes.toml` and `project-map.toml`, and the call is shown to load
//! nothing.
//! Test: this module.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use trusty_common::search_rpc::ProjectHealthCounts;
use trusty_common::uds::server::{RpcRouter, CODE_INTERNAL_ERROR};

use super::METHOD_PROJECT_HEALTH;
use crate::core::indexer::CodeIndexer;
use crate::core::project_map::{ParkReason, ParkedIndex, ProjectMap};
use crate::core::project_map_io::map_path_beside;
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
use crate::service::persistence::PersistedIndex;
use crate::service::rpc::project_health;
use crate::service::rpc::test_support::{rpc_err, rpc_ok};
use crate::service::server::SearchAppState;

/// A registry of four rows — one resident, three cold — and, when `map` is
/// given, that map beside it.
struct Fixture {
    _tmp: tempfile::TempDir,
    registry_toml: PathBuf,
    state: Arc<SearchAppState>,
    router: RpcRouter,
}

fn row(id: &str, root: &Path, queried: Option<u64>) -> PersistedIndex {
    PersistedIndex {
        colocated: true,
        last_queried_unix: queried,
        ..PersistedIndex::new(id, root)
    }
}

fn parked(id: &str, reason: ParkReason) -> ParkedIndex {
    ParkedIndex {
        index_id: id.to_string(),
        key: format!("root:/gone/{id}"),
        root_path: PathBuf::from(format!("/gone/{id}")),
        reason,
        parked_unix: 1,
    }
}

/// The map every count test uses: one orphaned and one decommissioned park
/// of registered indexes, plus a stale row for an unregistered id.
fn fixture_map() -> ProjectMap {
    ProjectMap {
        parked: vec![
            parked("parked-orphan", ParkReason::Orphaned),
            parked("parked-decom", ParkReason::Decommissioned),
            parked("ghost", ParkReason::Orphaned),
        ],
        ..ProjectMap::default()
    }
}

fn fixture(map: Option<&str>) -> Fixture {
    let tmp = tempfile::tempdir().expect("tempdir");
    let live = tmp.path().join("live");
    std::fs::create_dir_all(&live).expect("live root");
    let rows = [
        row("live", &live, Some(100)),
        row("parked-orphan", Path::new("/gone/parked-orphan"), Some(50)),
        row("parked-decom", Path::new("/gone/parked-decom"), None),
        row("cold-never", &tmp.path().join("cold"), None),
    ];
    let registry_toml = tmp.path().join("indexes.toml");
    crate::service::persistence::save_index_registry_at(&registry_toml, &rows)
        .expect("seed indexes.toml");
    if let Some(text) = map {
        std::fs::write(map_path_beside(&registry_toml), text).expect("seed project-map.toml");
    }
    let registry = IndexRegistry::new();
    registry.register(IndexHandle::bare(
        IndexId::new("live".to_string()),
        Arc::new(tokio::sync::RwLock::new(CodeIndexer::new("live", &live))),
        live,
    ));
    let state = Arc::new(SearchAppState::new(registry).with_registry_path(registry_toml.clone()));
    let router = project_health::register(RpcRouter::new(), &state);
    Fixture {
        _tmp: tmp,
        registry_toml,
        state,
        router,
    }
}

fn map_text() -> String {
    toml::to_string(&fixture_map()).expect("encode the map")
}

/// Why: each count follows its documented definition — `parked` and
/// `orphaned` only for registered ids, `orphaned` from the stored reason
/// alone, `never_queried` from an absent stamp.
/// Test: this test.
#[tokio::test]
async fn project_health_counts_follow_stored_state() {
    let fx = fixture(Some(&map_text()));
    let body = rpc_ok(&fx.router, METHOD_PROJECT_HEALTH, serde_json::Value::Null).await;
    let counts: ProjectHealthCounts = serde_json::from_value(body).expect("decode the counts");
    assert_eq!(
        counts,
        ProjectHealthCounts {
            registered: 4,
            resident: 1,
            parked: 2,
            orphaned: 1,
            never_queried: 2,
        }
    );
}

/// Why: criterion 1 — the call must load no corpus, so a parked or cold index
/// stays unloaded and the resident count is unchanged after it.
/// Test: this test.
#[tokio::test]
async fn project_health_loads_no_corpus() {
    let fx = fixture(Some(&map_text()));
    let before = fx.state.registry.len();
    let body = rpc_ok(&fx.router, METHOD_PROJECT_HEALTH, serde_json::json!({})).await;
    assert_eq!(body["resident"], 1);
    assert_eq!(fx.state.registry.len(), before, "the call loaded an index");
    for cold in ["parked-orphan", "parked-decom", "cold-never"] {
        assert!(
            fx.state
                .registry
                .get(&IndexId::new(cold.to_string()))
                .is_none(),
            "{cold} was loaded by a read-only count"
        );
    }
}

/// Why: a read-only call must not migrate a missing map (that is a write);
/// with no map nothing is parked.
/// Test: this test.
#[tokio::test]
async fn a_missing_project_map_counts_nothing_parked_and_writes_no_map() {
    let fx = fixture(None);
    let body = rpc_ok(&fx.router, METHOD_PROJECT_HEALTH, serde_json::Value::Null).await;
    assert_eq!(body["registered"], 4);
    assert_eq!(body["parked"], 0);
    assert_eq!(body["orphaned"], 0);
    assert!(
        !map_path_beside(&fx.registry_toml).exists(),
        "search.project.health wrote a project map"
    );
}

/// Why: an unreadable map must be an error naming the file, never zero
/// counts that tm would render as healthy.
/// Test: this test.
#[tokio::test]
async fn an_unreadable_project_map_is_an_internal_error_not_zero_counts() {
    let fx = fixture(Some("schema = \"not a number\"\n[[parked"));
    let err = rpc_err(&fx.router, METHOD_PROJECT_HEALTH, serde_json::Value::Null).await;
    assert_eq!(err.code, CODE_INTERNAL_ERROR);
    assert!(
        err.message.contains("project-map.toml"),
        "the error names the file: {}",
        err.message
    );
}
