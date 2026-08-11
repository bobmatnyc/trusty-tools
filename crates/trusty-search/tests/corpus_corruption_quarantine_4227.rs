//! Issue #4227 regression tests: a GENUINELY CORRUPT durable corpus must enter
//! the #4122 write quarantine instead of being recreated empty and served as
//! healthy.
//!
//! Why: `open_corpus_db_or_recreate` classified `Storage(Corrupted(_))`,
//! `RepairAborted` and `Storage(Io(InvalidData))` as recoverable alongside
//! `UpgradeRequired`. All four moved the file aside and returned `Ok` with a
//! fresh EMPTY corpus, so `build_indexer_from_entry` took its success arm,
//! wired the empty store, and left `corpus_open_failed` false. The index came
//! up reporting HEALTHY with 0 chunks and an ACTIVE watcher — so ordinary file
//! saves built a fresh PARTIAL corpus over the recreated one and persisted it,
//! which is the #4122 data-loss shape exactly. Nothing in the #4122 quarantine
//! or the #4087 query-surface guards could fire, because both key on
//! `corpus_open_failed` and corruption never set it.
//!
//! The split this pins: `UpgradeRequired` is a KNOWN old on-disk format with a
//! data-preserving recovery tool (`trusty-search migrate-redb`), so it must
//! keep recreating silently — quarantining it would turn every redb-2.x
//! upgrade into an outage. Genuine corruption has no such story and must stop.
//!
//! What: three tests.
//!   1. `genuine_corruption_quarantines_the_index` — the classification fix at
//!      the loader boundary, with the backup asserted so the fix is provably
//!      non-destructive.
//!   2. `corrupt_corpus_refuses_watcher_writes_and_chunk_count_stays_zero` —
//!      the data-loss sequence end to end through the real `spawn_watch_loop`
//!      and OS watcher, with a healthy sibling index as the positive control.
//!   3. `corruption_quarantine_preserves_the_original_corpus_bytes` — the
//!      moved-aside file still holds the original bytes, so an operator can
//!      still attempt recovery.
//!
//! Both 1 and 2 fail against pre-fix code for the right reason: pre-fix the
//! index is NOT quarantined, so #1 fails its `corpus_open_failed` assertion and
//! #2 watches `chunk_count` climb past 0 exactly as it did in production.
//!
//! Test: `cargo test -p trusty-search --test corpus_corruption_quarantine_4227`

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tempfile::tempdir;
use tokio::sync::RwLock;
use trusty_common::embedder::MockEmbedder;
use trusty_search::core::corpus::CorpusOpenFailure;
use trusty_search::core::Embedder;
use trusty_search::service::indexed_files::IndexedFiles;
use trusty_search::service::persistence::PersistedIndex;
use trusty_search::service::persistence_loader::build_indexer_from_entry;
use trusty_search::service::watch_loop::spawn_watch_loop;

/// Upper bound for every condition wait in this file.
///
/// Why: the watcher test depends on OS filesystem notifications plus the 500 ms
/// debounce window, so it needs a real wall-clock budget — but the waits are
/// condition-based, not fixed sleeps, so a healthy machine finishes well inside
/// a second.
const WAIT_BUDGET: Duration = Duration::from_secs(30);

fn mock_embedder() -> Arc<dyn Embedder> {
    Arc::new(MockEmbedder::new(8))
}

/// Build a colocated `PersistedIndex` entry rooted at `root`.
fn entry_at(id: &str, root: &Path) -> PersistedIndex {
    let mut entry = PersistedIndex::new(id.to_string(), root.to_path_buf());
    entry.colocated = true;
    entry
}

/// Path of the colocated `index.redb` for a root, matching
/// `persistence::corpus_redb_path_for_entry`'s colocated layout.
fn colocated_redb_path(root: &Path) -> PathBuf {
    root.join(".trusty-search").join("index.redb")
}

/// Write a genuinely CORRUPT `index.redb` at the colocated path.
///
/// Why: the fixture must be corruption, not an old format. Bytes that do not
/// parse as a redb database at all make `Database::create` return
/// `Storage(Io(InvalidData))` — the "this is not a redb file" signal — on every
/// OS, with no redb-2.x fixture to check in and no Unix-only permission modes.
/// What: creates `<root>/.trusty-search/` and fills `index.redb` with 4 KiB of
/// non-redb bytes. Returns the path written.
fn corrupt_the_corpus(root: &Path) -> PathBuf {
    let path = colocated_redb_path(root);
    std::fs::create_dir_all(path.parent().expect("redb path has a parent"))
        .expect("create colocated dir");
    std::fs::write(&path, [0xABu8; 4096]).expect("write corrupt corpus");
    path
}

/// Write `content` to `<root>/src/<name>` — a plain source save, the exact
/// event shape that destroyed the production corpus in #4122.
fn save_source_file(root: &Path, name: &str, content: &str) {
    let src = root.join("src");
    std::fs::create_dir_all(&src).expect("create src dir");
    std::fs::write(src.join(name), content).expect("write source file");
}

/// Poll `cond` while RE-SAVING `name` into every root in `roots`.
///
/// Why: a single save is a ONE-SHOT event. If OS watch registration has not
/// finished when it lands the event is lost permanently and no later one
/// arrives, so the race becomes a hang rather than a failure. Re-saving keeps
/// producing events for the whole budget, which STRENGTHENS the quarantine
/// assertion: the corrupt index gets MORE chances to wrongly accept a write,
/// not fewer, and the control must still demonstrably index before the test
/// proceeds.
/// What: polls `cond` every 25 ms and rewrites the file once per 500 ms
/// debounce window with CHANGING content, so no content-equality dedupe can
/// swallow the rewrite.
async fn await_condition_resaving<F, Fut>(roots: &[&Path], name: &str, mut cond: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = Instant::now() + WAIT_BUDGET;
    let mut last_save: Option<Instant> = None;
    let mut nonce = 0u32;
    while Instant::now() < deadline {
        if cond().await {
            return true;
        }
        let due = last_save.is_none_or(|t| t.elapsed() >= Duration::from_millis(500));
        if due {
            nonce += 1;
            for root in roots {
                save_source_file(root, name, &format!("pub fn probe_{nonce}() {{}}\n"));
            }
            last_save = Some(Instant::now());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    cond().await
}

/// Issue #4227 — corruption must reach the quarantine.
///
/// Why: this is the defect verbatim. Pre-fix, a corrupt `index.redb` was moved
/// aside, replaced with a fresh empty corpus, and WIRED — so
/// `corpus_open_failed` stayed false and the index presented as healthy. Every
/// downstream guard (#4122 write quarantine, #4087 query-surface 503) keys on
/// that flag, so none of them could fire for the one failure mode that is
/// permanent.
/// What: builds an indexer from an entry whose colocated `index.redb` is
/// garbage, then asserts the index is quarantined, classified
/// `FormatIncompatible` (so `/health` and the status endpoint say "rebuild",
/// not "wait"), reports the failure as non-transient, and that the corrupt
/// bytes were moved aside rather than deleted.
/// Test: this IS the test. Against pre-fix code `corpus_open_failed` is false.
// `multi_thread` because `build_indexer_from_entry` reaches `open_serialized`,
// which parks the blocking redb open on `spawn_blocking`; a current-thread
// runtime can starve that bridge.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn genuine_corruption_quarantines_the_index() {
    let dir = tempdir().expect("tempdir");
    let root = dir.path().to_path_buf();
    let redb_path = corrupt_the_corpus(&root);

    let indexer = build_indexer_from_entry(&entry_at("corrupt-4227", &root), &mock_embedder())
        .await
        .expect("building an indexer must not fail even when the corpus is corrupt");

    assert!(
        indexer.corpus_open_failed,
        "a GENUINELY CORRUPT corpus must set corpus_open_failed — pre-fix it was \
         recreated empty and wired, leaving the index reporting healthy (#4227)"
    );
    assert!(
        indexer.is_write_quarantined(),
        "corruption must put the index into the #4122 write quarantine so the \
         watcher cannot build a partial corpus over the recreated one (#4227)"
    );
    assert_eq!(
        indexer.corpus_open_failure,
        Some(CorpusOpenFailure::FormatIncompatible),
        "corruption is the ONE kind for which a rebuild is the correct remedy, so \
         it must classify as FormatIncompatible and not inherit the transient \
         'do not reindex' wording (#4333)"
    );
    assert!(
        !CorpusOpenFailure::FormatIncompatible.is_transient(),
        "a corrupt corpus never self-heals; reporting it as transient would tell \
         an operator to wait forever"
    );
    assert_eq!(
        indexer.chunk_count(),
        0,
        "the corrupt corpus contributes no chunks"
    );

    let backup = redb_path.with_file_name("index.redb.v2-incompatible");
    assert!(
        backup.exists(),
        "the corrupt bytes must be moved aside, not deleted — quarantine is a \
         stop, not a destructive repair"
    );
    assert!(
        !redb_path.exists(),
        "the corrupt file must be off the canonical path so the next boot opens \
         a clean corpus and boot reconcile can rebuild it"
    );
}

/// Issue #4227 — THE DATA-LOSS SEQUENCE.
///
/// Why: the quarantine flag only matters if it actually stops the watcher. This
/// reproduces the production shape end to end: a corrupt corpus is recreated
/// empty, the watcher stays live, and unrelated file saves populate a fresh
/// PARTIAL corpus that then persists over the recreated one. That is #4122's
/// incident with corruption as the trigger instead of an I/O error.
/// What: boots TWO real watch loops on two temp roots — one with a corrupt
/// corpus, one healthy. Saves an identical file into both. The healthy index is
/// the positive control: waiting for ITS `chunk_count` to grow proves the
/// watcher pipeline, the debounce window, and OS notification all work here, so
/// a flat `chunk_count` on the corrupt index is a refusal and not merely a slow
/// watcher. Waiting for the corrupt index's refusal counter to move proves the
/// event actually reached the guard.
/// Test: this IS the test. Against pre-fix code the corrupt index's
/// `chunk_count()` grows past 0 exactly as the production index's did.
// `multi_thread` is REQUIRED, not stylistic: the watcher bridges its OS
// notification thread into the async loop, and on a current-thread runtime that
// pipeline can starve — which is how the sibling #4122 test passed on macOS and
// failed on Linux CI.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn corrupt_corpus_refuses_watcher_writes_and_chunk_count_stays_zero() {
    let corrupt_dir = tempdir().expect("tempdir");
    let healthy_dir = tempdir().expect("tempdir");
    let corrupt_root = corrupt_dir.path().to_path_buf();
    let healthy_root = healthy_dir.path().to_path_buf();
    let embedder = mock_embedder();

    corrupt_the_corpus(&corrupt_root);

    let corrupt =
        build_indexer_from_entry(&entry_at("corrupt-4227-watch", &corrupt_root), &embedder)
            .await
            .expect("build indexer over corrupt corpus");
    let healthy = build_indexer_from_entry(&entry_at("healthy-4227", &healthy_root), &embedder)
        .await
        .expect("build healthy indexer");

    assert!(
        corrupt.is_write_quarantined(),
        "precondition: a corrupt corpus must quarantine the index (#4227)"
    );
    assert!(
        !healthy.is_write_quarantined(),
        "precondition: the control index must open its corpus cleanly"
    );
    assert_eq!(
        corrupt.chunk_count(),
        0,
        "precondition: corrupt index is empty"
    );

    let corrupt = Arc::new(RwLock::new(corrupt));
    let healthy = Arc::new(RwLock::new(healthy));

    let _corrupt_watch = spawn_watch_loop(&corrupt_root, Arc::clone(&corrupt), IndexedFiles::new())
        .expect("spawn corrupt watch loop");
    let _healthy_watch = spawn_watch_loop(&healthy_root, Arc::clone(&healthy), IndexedFiles::new())
        .expect("spawn healthy watch loop");

    let roots = [corrupt_root.as_path(), healthy_root.as_path()];

    // Positive control FIRST: prove the watcher pipeline works in this
    // environment before drawing any conclusion from the corrupt index.
    let healthy_indexed = {
        let healthy = Arc::clone(&healthy);
        await_condition_resaving(&roots, "probe.rs", move || {
            let healthy = Arc::clone(&healthy);
            async move { healthy.read().await.chunk_count() > 0 }
        })
        .await
    };
    assert!(
        healthy_indexed,
        "control index never indexed the saved file — the watcher pipeline is not \
         working in this environment, so no conclusion can be drawn about the \
         quarantined index"
    );

    // The corrupt index must have SEEN the event and refused it.
    let refused = {
        let corrupt = Arc::clone(&corrupt);
        await_condition_resaving(&roots, "probe.rs", move || {
            let corrupt = Arc::clone(&corrupt);
            async move { corrupt.read().await.refused_incremental_writes() > 0 }
        })
        .await
    };
    assert!(
        refused,
        "the watcher event never reached the quarantine guard on the corrupt \
         index — the refusal counter stayed at 0"
    );

    assert_eq!(
        corrupt.read().await.chunk_count(),
        0,
        "chunk_count on a corpus-corrupt index MUST stay 0. Any growth is a fresh \
         PARTIAL corpus being built over the recreated one — the #4122 data-loss \
         shape, reached through corruption (#4227)"
    );
}

/// Issue #4227 — the quarantine must not destroy the operator's recovery source.
///
/// Why: refusing writes is only half of "non-destructive". If the corrupt bytes
/// were deleted rather than moved aside, an operator would have nothing left to
/// attempt a salvage from, and the quarantine would have converted a recoverable
/// outage into a permanent one — the very trade this whole issue family exists
/// to prevent.
/// What: writes recognisable bytes as the corpus, builds the indexer, and
/// asserts the backup file holds those exact bytes.
/// Test: this IS the test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn corruption_quarantine_preserves_the_original_corpus_bytes() {
    let dir = tempdir().expect("tempdir");
    let root = dir.path().to_path_buf();
    let redb_path = colocated_redb_path(&root);
    std::fs::create_dir_all(redb_path.parent().expect("parent")).expect("create colocated dir");

    // Recognisable, deliberately non-redb bytes.
    let original: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(&redb_path, &original).expect("write corrupt corpus");

    let indexer =
        build_indexer_from_entry(&entry_at("corrupt-4227-bytes", &root), &mock_embedder())
            .await
            .expect("build indexer");
    assert!(
        indexer.is_write_quarantined(),
        "precondition: corruption must quarantine (#4227)"
    );

    let backup = redb_path.with_file_name("index.redb.v2-incompatible");
    assert_eq!(
        std::fs::read(&backup).expect("read backup"),
        original,
        "the moved-aside corpus must be byte-identical to the original — it is \
         the operator's only recovery source"
    );
}
