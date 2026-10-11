//! #9275 PR B: the dream compact pass over `{uuid}#n` chunk rows.
//!
//! Why: the compact pass rebuilds the whole index when it holds more than
//! `2 * drawers + 1` entries. Counting chunk ROWS there makes every healthy
//! chunked palace look orphan-ridden, and the rebuild re-embeds one vector per
//! drawer, dropping every chunk.
//! What: palaces with the mock embedder; chunk rows added with the raw
//! `HnswStore::upsert` primitive; `compact_pass` driven directly.
//! Test: this file IS the tests.

use super::cycle::compact_pass;
use crate::memory_core::palace::{Palace, PalaceId, RoomType};
use crate::memory_core::retrieval::{PalaceHandle, seed_shared_embedder_with_mock};
use crate::memory_core::store::kg_store::{VECTOR_KEYS, VECTORS};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing_subscriber::fmt::MakeWriter;
use uuid::Uuid;

fn open_handle(dir: &std::path::Path, name: &str) -> Arc<PalaceHandle> {
    seed_shared_embedder_with_mock();
    let palace = Palace {
        id: PalaceId::new(name),
        name: name.into(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: dir.join(name),
    };
    std::fs::create_dir_all(&palace.data_dir).unwrap();
    PalaceHandle::open(&palace).unwrap()
}

async fn remember(handle: &PalaceHandle, text: &str) -> Uuid {
    handle
        .remember(text.into(), RoomType::General, vec![], 0.5)
        .await
        .unwrap()
}

fn add_chunk_rows(handle: &PalaceHandle, id: Uuid, extra: usize) {
    let dim = handle.vector_store.dim();
    for n in 1..=extra {
        let mut v = vec![0.0f32; dim];
        v[n % dim] = 1.0;
        handle
            .vector_store
            .hnsw_for_test()
            .upsert(&format!("{id}#{n}"), &v)
            .expect("raw chunk row");
    }
}

fn keys_of(handle: &PalaceHandle, id: Uuid) -> usize {
    let prefix = id.to_string();
    handle
        .vector_store
        .hnsw_for_test()
        .all_keys()
        .expect("all_keys")
        .iter()
        .filter(|k| k.starts_with(&prefix))
        .count()
}

/// AC-B14: three drawers of four rows each is a healthy palace. The compact
/// pass removes nothing and does not reset and rebuild the index.
#[tokio::test]
async fn drawer_count_ignores_chunk_rows_and_dream_does_not_rebuild_a_chunked_palace() {
    let dir = tempfile::tempdir().unwrap();
    let handle = open_handle(dir.path(), "chunk-dream");
    let mut ids = Vec::new();
    for text in [
        "Sloths digest leaves slowly",
        "Ravens solve multi-step puzzles",
        "Mantis shrimp see polarised light",
    ] {
        let id = remember(&handle, text).await;
        add_chunk_rows(&handle, id, 3);
        ids.push(id);
    }
    assert_eq!(handle.vector_store.index_size(), 12);

    let removed = compact_pass(&handle, Instant::now(), Duration::from_secs(30))
        .await
        .unwrap();
    assert_eq!(removed, 0, "nothing in a healthy palace is an orphan");
    assert_eq!(
        handle.vector_store.index_size(),
        12,
        "a rebuild would have re-embedded one row per drawer"
    );
    for id in ids {
        assert_eq!(keys_of(&handle, id), 4, "every chunk row survives");
    }
}

/// Fail-open check 2 (`cycle.rs` compact pass, #9275): when removing an orphan
/// drawer's vectors fails, the pass counts no removal, drops no row, and the
/// orphan's every chunk row stays reclaimable by `compact_vector_orphans`.
#[tokio::test]
async fn dream_compact_counts_only_successful_removals_of_a_chunked_orphan() {
    let dir = tempfile::tempdir().unwrap();
    let handle = open_handle(dir.path(), "chunk-dream-fail");
    let kept = remember(&handle, "Elephants mourn their dead").await;
    let orphan = remember(&handle, "Bees dance to give directions").await;
    add_chunk_rows(&handle, orphan, 2);
    handle.drawers.write().retain(|d| d.id != orphan);
    let _ = handle.kg.delete_drawer(orphan).await;
    // Every bounded vector call fails from here on.
    handle.vector_store.op_breaker().trip_for_test();

    let removed = compact_pass(&handle, Instant::now(), Duration::from_secs(30))
        .await
        .unwrap();
    assert_eq!(removed, 0, "a failed removal is not counted as one");
    assert_eq!(keys_of(&handle, orphan), 3, "no row is dropped on failure");
    assert_eq!(keys_of(&handle, kept), 1);

    let result = handle.compact_vector_orphans().await.unwrap();
    assert_eq!(result.orphans_removed, 3);
    assert_eq!(keys_of(&handle, orphan), 0, "every chunk row is reclaimed");
    assert_eq!(keys_of(&handle, kept), 1);
}

/// Collects formatted `warn` output from this thread.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Capture {
    type Writer = Capture;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Write rows straight into a palace's vector redb before it opens: each
/// `Some(key)` row is keyed, each `None` row has a vector and no key.
fn seed_vector_rows(data_dir: &std::path::Path, rows: &[(u64, Option<String>)]) {
    std::fs::create_dir_all(data_dir).unwrap();
    let db = redb::Database::create(data_dir.join("index.usearch.redb")).unwrap();
    let wtx = db.begin_write().unwrap();
    {
        let mut vectors = wtx.open_table(VECTORS).unwrap();
        let mut keys = wtx.open_table(VECTOR_KEYS).unwrap();
        for (id, key) in rows {
            let mut v = vec![0.0f32; 384];
            v[(*id as usize) % 384] = 1.0;
            let encoded = postcard::to_allocvec(&v).unwrap();
            vectors.insert(*id, encoded.as_slice()).unwrap();
            if let Some(key) = key {
                keys.insert(key.as_str(), *id).unwrap();
            }
        }
    }
    wtx.commit().unwrap();
}

/// AC-B7 (#9275): rows the addressable pass cannot enumerate — keys that name
/// no drawer, and vectors with no key — still count toward the open-time
/// orphan warning and the dream rebuild trigger, exactly as on main.
#[tokio::test]
async fn unparseable_and_keyless_rows_still_trip_the_orphan_warning_and_rebuild() {
    let dir = tempfile::tempdir().unwrap();
    let name = "legacy-orphans";
    let ghost = Uuid::new_v4();
    seed_vector_rows(
        &dir.path().join(name),
        &[
            (900_001, Some("not-a-drawer".into())),
            (900_002, Some(format!("{ghost}#0"))),
            (900_003, Some(format!("{ghost}#01"))),
            (900_004, None),
            (900_005, None),
            (900_006, None),
        ],
    );

    // Six entries against zero drawers: over the `drawers + 5` warning bar.
    let cap = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(cap.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::WARN)
        .finish();
    let guard = tracing::subscriber::set_default(subscriber);
    let handle = open_handle(dir.path(), name);
    drop(guard);
    let log = String::from_utf8_lossy(&cap.0.lock().unwrap()).into_owned();
    assert!(
        log.contains("orphaned entries"),
        "the open-time orphan warning fires: {log}"
    );

    // Seven entries against one drawer: over the `2 * drawers + 1` trigger.
    remember(&handle, "Octopuses taste with their arms").await;
    assert_eq!(handle.vector_store.index_size(), 7);
    let removed = compact_pass(&handle, Instant::now(), Duration::from_secs(30))
        .await
        .unwrap();
    assert_eq!(
        handle.vector_store.index_size(),
        1,
        "the rebuild reset the index and re-embedded the one drawer"
    );
    assert_eq!(removed, 6, "the rebuild drops every unenumerable entry");
}
