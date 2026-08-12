//! The file watcher must not orphan chunks a partial commit at the chunk cap
//! left behind (#100).
//!
//! Why: `CodeIndexer::index_file` now returns `Err` when `TRUSTY_MAX_CHUNKS`
//! discarded part of a file, and raises that refusal AFTER committing the
//! chunks that did fit. `handle_modified` logged and returned without calling
//! `indexed_files.record(...)`, so those committed chunks became invisible to
//! the watcher's own cleanup: a later delete found no `IndexedFiles` entry and
//! `handle_removed` returned early, and a later edit's stale-removal pass was
//! skipped the same way. The chunks stayed in the corpus, BM25 and HNSW for the
//! index's lifetime.
//!
//! What: drives `handle_modified` / `handle_removed` directly against a real
//! `CodeIndexer` pinned at a cap that admits exactly one more chunk, so a
//! two-chunk file lands partly. Calling them directly rather than through
//! `spawn_watch_loop` is deliberate: the sequence under test is a specific
//! corpus state, and racing real OS events plus a 500 ms debouncer to reach it
//! would make the test flaky without testing anything more.
//!
//! Its own test binary because `TRUSTY_MAX_CHUNKS` is process-global and
//! `max_chunks_per_index()` re-reads it on every call. `set_var` mutates the
//! process `environ`, which can reallocate under a concurrent reader in another
//! thread (`a91cae795` / #3769) — `#[serial_test::serial]` orders the tests in
//! THIS binary against each other, and the separate binary keeps them away from
//! everything else.
//!
//! Test: `cargo test -p trusty-search --test watcher_chunk_cap_orphans_100`

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::RwLock;
use trusty_search::core::indexer::CodeIndexer;
use trusty_search::core::registry::IndexId;
use trusty_search::service::indexed_files::IndexedFiles;
use trusty_search::service::watch_loop::{handle_modified, handle_removed};

/// Restore `TRUSTY_MAX_CHUNKS` to its prior value however the test ends.
struct MaxChunksEnvGuard(Option<String>);

impl MaxChunksEnvGuard {
    fn set(value: usize) -> Self {
        let prior = std::env::var("TRUSTY_MAX_CHUNKS").ok();
        // SAFETY: serialized against every other reader/writer of this var by
        // `#[serial_test::serial]` on each test, and this binary holds only
        // those tests.
        unsafe { std::env::set_var("TRUSTY_MAX_CHUNKS", value.to_string()) };
        Self(prior)
    }

    fn set_to(&self, value: usize) {
        // SAFETY: same serialization as `set`.
        unsafe { std::env::set_var("TRUSTY_MAX_CHUNKS", value.to_string()) };
    }
}

impl Drop for MaxChunksEnvGuard {
    fn drop(&mut self) {
        // SAFETY: same serialization as `set`.
        unsafe {
            match self.0.take() {
                Some(v) => std::env::set_var("TRUSTY_MAX_CHUNKS", v),
                None => std::env::remove_var("TRUSTY_MAX_CHUNKS"),
            }
        }
    }
}

const BASELINE: &str = "pub fn baseline_one() {}\npub fn baseline_two() {}\n";
/// Two top-level items, so the cap set below admits the first and rejects the
/// rest — the documented partial-fit case.
const PARTIAL_V1: &str = "pub fn partial_one() {}\npub fn partial_two() {}\n";
/// Same two items shifted down by one line. Every chunk id ends in its start
/// line (`{file}::{chunk_type}::{name}::{start_line}`, or `{file}:{start}:{end}`
/// for an unnamed chunk — `chunker::walk::make_chunk_id`), so no id here
/// collides with a `PARTIAL_V1` id.
const PARTIAL_V2: &str = "// edited\npub fn partial_one() {}\npub fn partial_two() {}\n";

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    index_id: IndexId,
    indexer: Arc<RwLock<CodeIndexer>>,
    indexed_files: IndexedFiles,
}

impl Fixture {
    /// Index `baseline.rs` with the cap wide open, then pin the cap one chunk
    /// above the resulting count so the next file lands partly.
    async fn at_cap_with_one_slot_free() -> (Self, MaxChunksEnvGuard) {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(dir.path()).expect("canonicalize root");
        let guard = MaxChunksEnvGuard::set(1_000_000);

        let index_id = IndexId::new("cap-watch-test");
        let indexer = Arc::new(RwLock::new(CodeIndexer::new(
            "cap-watch-test",
            root.clone(),
        )));
        let fixture = Fixture {
            _dir: dir,
            root,
            index_id,
            indexer,
            indexed_files: IndexedFiles::new(),
        };

        fixture.write("baseline.rs", BASELINE);
        fixture.modified("baseline.rs").await;
        let baseline = fixture.chunk_count().await;
        assert!(baseline > 0, "baseline fixture must produce chunks");

        guard.set_to(baseline + 1);
        (fixture, guard)
    }

    fn write(&self, name: &str, content: &str) {
        std::fs::write(self.root.join(name), content).expect("write fixture file");
    }

    async fn modified(&self, name: &str) {
        handle_modified(
            &self.root.join(name),
            &self.index_id,
            &self.root,
            &self.root,
            &self.indexer,
            &self.indexed_files,
        )
        .await;
    }

    async fn removed(&self, name: &str) {
        handle_removed(
            &self.root.join(name),
            &self.index_id,
            &self.root,
            &self.root,
            &self.indexer,
            &self.indexed_files,
        )
        .await;
    }

    async fn chunk_count(&self) -> usize {
        self.indexer.read().await.in_memory_chunk_count().await
    }

    /// Ids of every chunk the corpus holds for `file`.
    async fn ids_for(&self, file: &str) -> Vec<String> {
        let mut ids: Vec<String> = self
            .indexer
            .read()
            .await
            .raw_chunks_snapshot()
            .await
            .into_iter()
            .filter(|c| c.file == file)
            .map(|c| c.id)
            .collect();
        ids.sort();
        ids
    }
}

/// The delete half of the leak: a file that landed partly at the cap must still
/// be evictable by a later `Removed` event.
///
/// Pre-fix `handle_modified` recorded nothing on the new cap `Err`, so
/// `handle_removed`'s `IndexedFiles::take` returned `None` and it returned
/// early — the chunks that did commit stayed in the corpus permanently. Only
/// `POST /remove-file` or a full reindex, which scan the corpus directly rather
/// than through `IndexedFiles`, could ever have cleaned them up.
#[tokio::test]
#[serial_test::serial]
async fn partial_commit_at_cap_then_delete_leaves_no_orphan_chunks() {
    let (fx, _guard) = Fixture::at_cap_with_one_slot_free().await;
    let baseline = fx.chunk_count().await;

    fx.write("partial.rs", PARTIAL_V1);
    fx.modified("partial.rs").await;

    let landed = fx.ids_for("partial.rs").await;
    assert_eq!(
        landed.len(),
        1,
        "fixture must land exactly one of the file's chunks and have the cap drop the rest, \
         got {landed:?}"
    );
    assert_eq!(fx.chunk_count().await, baseline + 1);

    std::fs::remove_file(fx.root.join("partial.rs")).expect("delete fixture file");
    fx.removed("partial.rs").await;

    assert!(
        fx.ids_for("partial.rs").await.is_empty(),
        "the chunk that committed before the cap refusal must be evicted on delete, not orphaned"
    );
    assert_eq!(
        fx.chunk_count().await,
        baseline,
        "the corpus must be back to its pre-write size"
    );
}

/// The edit half of the leak: the stale-removal pass at the top of
/// `handle_modified` is driven by `IndexedFiles`, so an unrecorded partial
/// commit is never superseded.
///
/// Pre-fix the v1 chunk stayed in the corpus under its old positional id — and
/// because it kept occupying the index's last free slot, the edited content
/// could not land either. The file was frozen at a stale partial version with
/// no way back.
#[tokio::test]
#[serial_test::serial]
async fn partial_commit_at_cap_then_edit_replaces_the_landed_chunk() {
    let (fx, _guard) = Fixture::at_cap_with_one_slot_free().await;
    let baseline = fx.chunk_count().await;

    fx.write("partial.rs", PARTIAL_V1);
    fx.modified("partial.rs").await;
    let v1_ids = fx.ids_for("partial.rs").await;
    assert_eq!(v1_ids.len(), 1, "fixture must land exactly one chunk");

    fx.write("partial.rs", PARTIAL_V2);
    fx.modified("partial.rs").await;
    let v2_ids = fx.ids_for("partial.rs").await;

    assert_eq!(
        v2_ids.len(),
        1,
        "the edit must land, which requires the stale chunk to have freed the slot first"
    );
    assert_ne!(
        v2_ids, v1_ids,
        "the stale chunk from the partial commit must be gone: a chunk id ends in its start \
         line, and the edit moved every item down one"
    );
    assert_eq!(
        fx.chunk_count().await,
        baseline + 1,
        "the edit must replace the stale chunk, not accumulate beside it"
    );
}
