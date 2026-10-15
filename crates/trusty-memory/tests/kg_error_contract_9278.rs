//! #9278 T1a/T1b-2: the `KgStoreRedb` and `KnowledgeGraph` paths, room
//! resolution and the compaction fault hook return the typed `KgError`.
//!
//! Why: a signature check that compiles on both sides of the change turns red
//! at run time on the pre-change code, so the conversion is proven per path.
//! What: reads the error type of each call's `Result` by name.
//! Test: `kg_store_redb_paths_return_kg_error`,
//! `knowledge_graph_paths_return_kg_error`,
//! `compact_fault_hook_error_type_is_kg_error`.

use std::sync::Arc;
use tempfile::tempdir;
use trusty_memory::engine::palace::{Drawer, RoomType};
use trusty_memory::engine::store::kg::{KnowledgeGraph, Triple};
use trusty_memory::engine::store::kg_redb::copy_swap::{CompactFaultHook, CompactStep};
use trusty_memory::engine::store::kg_redb::{BatchWriteOp, KgStoreRedb};
use trusty_memory::engine::store::rooms::resolve_or_create_room_sync;
use uuid::Uuid;

/// The error type name of a `Result`, read without naming the type.
fn err_type<T, E>(_: &Result<T, E>) -> &'static str {
    std::any::type_name::<E>()
}

#[test]
fn kg_store_redb_paths_return_kg_error() {
    let dir = tempdir().expect("tempdir");
    let opened = KgStoreRedb::open(&dir.path().join("kg.redb"));
    let open_type = err_type(&opened);
    let store = Arc::new(opened.expect("open"));
    let drawer = Drawer::new(Uuid::new_v4(), "contract");
    let room = resolve_or_create_room_sync(&store, &RoomType::Custom("contract".into()));
    let rows = [
        ("open", open_type),
        (
            "assert",
            err_type(&store.assert(&Triple::new("a", "b", "c"))),
        ),
        ("query_active", err_type(&store.query_active("a"))),
        (
            "count_active_triples",
            err_type(&store.count_active_triples()),
        ),
        ("upsert_drawer", err_type(&store.upsert_drawer(&drawer))),
        ("load_drawer_ids", err_type(&store.load_drawer_ids())),
        ("list_rooms", err_type(&store.list_rooms())),
        ("import_all", err_type(&store.import_all(vec![], vec![]))),
        (
            "apply_batch",
            err_type(&store.apply_batch(&[BatchWriteOp::UpsertDrawer(drawer.clone())])),
        ),
        ("resolve_or_create_room_sync", err_type(&room)),
    ];
    let wrong: Vec<_> = rows
        .iter()
        .filter(|(_, ty)| !ty.ends_with("::KgError"))
        .collect();
    assert!(wrong.is_empty(), "#9278: not KgError: {wrong:?}");
}

// #9278 T1b-2: the 14 `KnowledgeGraph` paths.
#[tokio::test]
async fn knowledge_graph_paths_return_kg_error() {
    let dir = tempdir().expect("tempdir");
    let opened = KnowledgeGraph::open(&dir.path().join("kg.db"));
    let open_type = err_type(&opened);
    let kg = opened.expect("open");
    let drawer = Drawer::new(Uuid::new_v4(), "contract");
    let rows = [
        ("open", open_type),
        (
            "assert",
            err_type(&kg.assert(Triple::new("a", "b", "c")).await),
        ),
        ("retract", err_type(&kg.retract("a", "b").await)),
        ("query_active", err_type(&kg.query_active("a").await)),
        ("list_subjects", err_type(&kg.list_subjects(8))),
        ("count_active_triples", err_type(&kg.count_active_triples())),
        ("neighbors", err_type(&kg.neighbors("a"))),
        ("shortest_path", err_type(&kg.shortest_path("a", "c"))),
        ("reachable", err_type(&kg.reachable("a", 2))),
        ("incoming", err_type(&kg.incoming("c"))),
        ("connected_components", err_type(&kg.connected_components())),
        ("astar_path", err_type(&kg.astar_path("a", "c"))),
        ("upsert_drawer", err_type(&kg.upsert_drawer(&drawer).await)),
        (
            "upsert_drawer_sync",
            err_type(&kg.upsert_drawer_sync(&drawer)),
        ),
    ];
    let wrong: Vec<_> = rows
        .iter()
        .filter(|(_, ty)| !ty.ends_with("::KgError"))
        .collect();
    assert!(wrong.is_empty(), "#9278: not KgError: {wrong:?}");
}

#[test]
fn compact_fault_hook_error_type_is_kg_error() {
    let hook: CompactFaultHook = Arc::new(|_| Ok(()));
    let ty = err_type(&hook(CompactStep::AfterBackup));
    assert!(ty.ends_with("::KgError"), "#9278: hook error is {ty}");
}
