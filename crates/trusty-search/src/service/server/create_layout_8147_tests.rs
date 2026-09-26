//! Regression tests for the #8147 final critic round: the `indexes.toml` row,
//! not the in-memory stores, decides an existing id's storage layout.
//!
//! Why: split from `tests_8147` to keep that file under the production SLOC
//! cap; it shares that module's request helpers.
//! What: the row-only id, the unreadable registry, the `409` ahead of the
//! warming embedder's `503`, and the `resolve_layout` table.
//! Test: this module. Run with `cargo test -p trusty-search create_layout_8147`.

use super::tests_8147::{create, create_req_with_colocated, mock_state};
use super::*;
use crate::core::registry::{IndexId, IndexRegistry};
use axum::http::StatusCode;
use std::sync::Arc;

/// Plant an `indexes.toml` row for `id` at `root`, with no in-memory handle
/// and no cold entry — the state a lazy load leaves between the handler's two
/// in-memory lookups, and the state of an id held in no store at all.
fn plant_row_only(id: &IndexId, root: &std::path::Path, colocated: bool) {
    crate::service::persistence::upsert_index_registry_entry(
        crate::service::persistence::PersistedIndex {
            id: id.0.clone(),
            root_path: root.to_path_buf(),
            colocated,
            ..Default::default()
        },
    )
    .expect("plant indexes.toml row");
}

/// #8147 final round, finding 1: an id whose only record is its `indexes.toml`
/// row keeps the row's layout.
///
/// Why: the handler read the layout from the live registry and then the cold
/// store. A lazy load between the two moves the id live and removes its cold
/// entry, so both miss and the request chose the layout, building an empty
/// corpus in the other layout. The row survives that load.
/// What: plants a `colocated=false` row with nothing in memory, then asserts
/// an explicit `colocated: true` is a `409` that creates nothing, and an
/// omitted field registers the data-dir layout with the row unchanged.
/// Test: this test. With the layout read from the cold store, the `true`
/// request answers `200` and the omitted one creates `<root>/.trusty-search/`.
#[tokio::test]
#[serial_test::serial]
async fn create_index_row_only_id_inherits_its_recorded_layout() {
    let _data_dir = super::tests_components::IsolatedDataDir::new();
    let state = mock_state().await;
    let (_dir, root) = super::test_support::allowlisted_index_root("ts-8147-row-");
    let id = IndexId::new("ts-8147-row");
    plant_row_only(&id, &root, false);

    let (status, body) = create(&state, &id, &root, Some(true)).await;
    assert_eq!(status, StatusCode::CONFLICT, "#8147: {body}");
    assert_eq!(body["registered_colocated"], false, "{body}");
    assert!(state.registry.get(&id).is_none(), "409 registers nothing");
    assert!(
        !root.join(".trusty-search").exists(),
        "409 creates nothing under the root"
    );

    let (status, body) = create(&state, &id, &root, None).await;
    assert_eq!(status, StatusCode::OK, "omitted field registers. {body}");
    assert!(
        !root.join(".trusty-search").exists(),
        "#8147: an omitted field must keep the row's data-dir layout"
    );
    let entry = crate::service::persistence::find_index_registry_entry(&id.0)
        .expect("registry readable")
        .expect("still registered");
    assert!(!entry.colocated, "indexes.toml keeps colocated=false");
    let handle = state.registry.get(&id).expect("resident");
    assert_eq!(
        crate::service::storage_layout::layout_of(&handle).await,
        crate::service::storage_layout::StorageLayout::DataDir,
    );

    state.watcher_manager.stop_for_index(&id).await;
}

/// #8147 final round, finding 1: a create whose `indexes.toml` cannot be read
/// is refused, never decided from the request.
///
/// Why: an unreadable registry says nothing about whether the id already
/// records a layout, so choosing one could orphan the real corpus.
/// What: arms a per-id read fault (a planted bad file would break concurrent
/// tests sharing `TRUSTY_DATA_DIR`) and asserts a `500` naming `indexes.toml`,
/// with no handle registered and no `.trusty-search/` created.
/// Test: this test. Falling back to `None` on the read error answers `200`.
#[tokio::test]
#[serial_test::serial]
async fn create_index_refuses_when_the_registry_cannot_be_read() {
    let _data_dir = super::tests_components::IsolatedDataDir::new();
    let state = mock_state().await;
    let (_dir, root) = super::test_support::allowlisted_index_root("ts-8147-noread-");
    let id = IndexId::new("ts-8147-noread");
    let _fault = super::create_layout::registry_fault::RegistryFault::arm_read(&id.0);

    let (status, body) = create(&state, &id, &root, None).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("indexes.toml"),
        "the error names the registry. Body: {body}"
    );
    assert!(state.registry.get(&id).is_none(), "nothing registered");
    assert!(
        !root.join(".trusty-search").exists(),
        "nothing created under the root"
    );
}

/// #8147 final round, finding 3: a layout `409` is not masked by the warming
/// embedder's retryable `503`.
///
/// Why: a caller that retries a `503` would retry a request that can never
/// succeed, and never see the conflict.
/// What: a state with no embedder installed, a planted `colocated=true` row,
/// and an explicit `colocated: false` at the same root → `409`.
/// Test: this test. With the layout resolved after `current_embedder()`, the
/// request answers `503 embedder_initializing`.
#[tokio::test]
#[serial_test::serial]
async fn create_index_layout_conflict_is_409_while_the_embedder_warms() {
    let _data_dir = super::tests_components::IsolatedDataDir::new();
    let state = Arc::new(SearchAppState::new(IndexRegistry::new()));
    assert!(state.current_embedder().await.is_none(), "precondition");
    let (_dir, root) = super::test_support::allowlisted_index_root("ts-8147-warm-");
    let id = IndexId::new("ts-8147-warm");
    plant_row_only(&id, &root, true);

    let (status, body) = create(&state, &id, &root, Some(false)).await;
    assert_eq!(status, StatusCode::CONFLICT, "#8147: {body}");
}

/// #8147 final round, finding 2: `resolve_layout` across request × record.
///
/// Why: an omitted field must inherit the recorded layout even at a new root,
/// as relocation keeps it (#1089); an explicit value decides at a new root.
/// What: table over (request root, `colocated`, recorded layout).
/// Test: this test. With the same-tree filter on the omitted arm, the
/// new-root omitted rows resolve to colocated.
#[test]
fn resolve_layout_inherits_or_decides_per_request_and_record() {
    let old = std::path::PathBuf::from("/tmp/ts-8147-resolve/old");
    let new = std::path::PathBuf::from("/tmp/ts-8147-resolve/new");
    let row = |colocated| crate::service::persistence::PersistedIndex {
        id: "ts-8147-resolve".into(),
        root_path: old.clone(),
        colocated,
        ..Default::default()
    };
    // (request root, requested, recorded, expected layout; `None` = 409)
    let cases = [
        (&new, None, None, Some(true)),
        (&new, Some(false), None, Some(false)),
        (&old, None, Some(false), Some(false)),
        (&new, None, Some(false), Some(false)),
        (&new, None, Some(true), Some(true)),
        (&new, Some(true), Some(false), Some(true)),
        (&new, Some(false), Some(true), Some(false)),
        (&old, Some(true), Some(true), Some(true)),
        (&old, Some(true), Some(false), None),
        (&old, Some(false), Some(true), None),
    ];
    for (root, asked, recorded, want) in cases {
        let req = create_req_with_colocated("ts-8147-resolve", root.clone(), asked);
        let recorded = recorded.map(row);
        let got = super::create_layout::resolve_layout(&req, recorded.as_ref());
        let recorded_layout = recorded.as_ref().map(|r| r.colocated);
        let context = format!("{root:?} asked={asked:?} recorded={recorded_layout:?}");
        match want {
            Some(layout) => assert_eq!(got.ok(), Some(layout), "{context}"),
            None => assert_eq!(
                got.err().map(|(status, _)| status),
                Some(StatusCode::CONFLICT),
                "{context}"
            ),
        }
    }
}
