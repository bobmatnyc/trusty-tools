//! Issue #4122 regression tests: an index whose durable corpus failed to open
//! must REFUSE incremental/watcher writes until a corpus open succeeds.
//!
//! Why: in production, `duettoresearch-hackathon-tm-hackathon-01` failed its
//! redb corpus open at warm boot (`corpus_open_failed: true`, `chunk_count:
//! 0`) but its file watcher stayed `active: true`. Ordinary, unrelated file
//! saves in the worktree kept flowing into the handle, so `chunk_count`
//! climbed `0 → 68 → 1334` and a FRESH, PARTIAL corpus was persisted over the
//! never-opened original — which is now unrecoverable. The sibling index
//! `cto-duetto` hit the identical open failure in the same boot, took no
//! watcher writes, and recovered all 200,090 chunks on the next restart.
//! Watcher-writes-during-failure was the ONLY difference between clean
//! recovery and permanent loss.
//!
//! What: three tests, mirroring the three halves of the fix.
//!   1. `quarantined_index_refuses_watcher_write_and_chunk_count_stays_zero`
//!      — the corruption sequence, end to end through the real
//!      `spawn_watch_loop` + OS watcher, with a healthy sibling index in the
//!      same test as the positive control.
//!   2. `successful_reopen_lifts_quarantine_and_leaves_corpus_intact`
//!      — the `cto-duetto` recovery case. Guards against over-refusal: a
//!      quarantine that never lifts is a new outage, not a fix.
//!   3. `quarantine_refusal_emits_error_level_diagnostic`
//!      — the refusal must reach a diagnostic surface operators actually
//!      read. In this workspace only ERROR-level events reach
//!      `errors.jsonl` / `list_recent_errors` / `tm doctor`, so this test
//!      asserts capture through the REAL `BugCaptureLayer`, not merely that
//!      some log line was emitted.
//!
//! Both failure modes are induced by production code paths rather than by
//! poking `corpus_open_failed` directly: tests 1 and 3 put a DIRECTORY where
//! `index.redb` belongs (portable I/O failure), and test 2 holds a live redb
//! handle so the second open fails with `DatabaseAlreadyOpen` — the shape
//! that leaves the original corpus fully intact on disk, exactly as in the
//! incident.
//!
//! Test: `cargo test -p trusty-search --test corpus_open_quarantine_4122`

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tempfile::tempdir;
use tokio::sync::RwLock;
use trusty_common::embedder::MockEmbedder;
use trusty_search::core::corpus::CorpusStore;
use trusty_search::core::registry::IndexId;
use trusty_search::core::{chunk_ast, Embedder};
use trusty_search::service::indexed_files::IndexedFiles;
use trusty_search::service::persistence::PersistedIndex;
use trusty_search::service::persistence_loader::build_indexer_from_entry;
use trusty_search::service::watch_loop::spawn_watch_loop;

/// Upper bound for every condition wait in this file.
///
/// Why: the watcher tests depend on OS filesystem notifications plus the
/// 500 ms debounce window, so they need a real wall-clock budget — but the
/// waits are condition-based (`await_condition_resaving`), not fixed sleeps,
/// so a healthy machine finishes in well under a second.
const WAIT_BUDGET: Duration = Duration::from_secs(30);

/// Poll `cond` while RE-SAVING `name` into every root in `roots`.
///
/// Why: a single save is a ONE-SHOT event. If the OS watch registration has
/// not finished when it lands — routine on a loaded CI runner, where this test
/// first failed by burning its full 30 s budget with a control that never
/// indexed — the event is lost permanently and no later event ever arrives, so
/// the race becomes a hang and a red build. Re-saving keeps producing events
/// for the whole budget, so the wait ends as soon as the watcher is genuinely
/// live rather than depending on a startup sleep being long enough.
///
/// This STRENGTHENS the quarantine assertions rather than weakening them: the
/// broken index gets MORE chances to wrongly accept a write, not fewer, and
/// the control must still demonstrably index before the test proceeds.
/// What: polls `cond` every 25 ms (so a healthy machine still finishes in well
/// under a second) and rewrites the file once per 500 ms debounce window with
/// CHANGING content, so no content-equality dedupe can swallow the rewrite.
async fn await_condition_resaving<F, Fut>(roots: &[&Path], name: &str, mut cond: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = Instant::now() + WAIT_BUDGET;
    let mut generation: u32 = 0;
    let mut last_save: Option<Instant> = None;
    while Instant::now() < deadline {
        if cond().await {
            return true;
        }
        if last_save.is_none_or(|t| t.elapsed() >= Duration::from_millis(500)) {
            generation += 1;
            let body = format!("pub fn quarantine_probe() -> u32 {{ 4122 + {generation} }}\n");
            for root in roots {
                save_source_file(root, name, &body);
            }
            last_save = Some(Instant::now());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    cond().await
}

fn mock_embedder() -> Arc<dyn Embedder> {
    Arc::new(MockEmbedder::new(8))
}

/// Build a colocated `PersistedIndex` entry rooted at `root`.
fn entry_at(id: &str, root: &Path) -> PersistedIndex {
    {
        let mut e = PersistedIndex::new(id.to_string(), root.to_path_buf());
        e.colocated = true;
        e
    }
}

/// Sabotage the colocated redb path so `CorpusStore::open` fails on every OS.
///
/// Why: a DIRECTORY where redb expects a plain file makes
/// `Database::create()` return an I/O error portably, without needing an
/// incompatible-format fixture or Unix-only permission modes. This is the
/// same technique the #1158 regression test uses.
/// What: creates `<root>/.trusty-search/index.redb` as a directory.
fn sabotage_corpus_path(root: &Path) {
    let colocated = root.join(".trusty-search");
    std::fs::create_dir_all(&colocated).expect("create colocated dir");
    std::fs::create_dir_all(colocated.join("index.redb")).expect("create dir at redb path");
}

/// Write `content` to `<root>/src/<name>` (creating `src/`) — a plain source
/// save, the exact event shape that destroyed the production corpus.
fn save_source_file(root: &Path, name: &str, content: &str) {
    let src = root.join("src");
    std::fs::create_dir_all(&src).expect("create src dir");
    std::fs::write(src.join(name), content).expect("write source file");
}

/// Issue #4122 — THE CORRUPTION SEQUENCE.
///
/// Why: this is the defect verbatim. An index whose corpus never opened keeps
/// a live watcher; a save to an unrelated file in the worktree is accepted and
/// starts building a brand-new partial corpus on top of the unopened one.
/// After the fix the watcher path must refuse the write and `chunk_count` must
/// stay at 0.
/// What: boots TWO real watch loops on two temp roots — one index sabotaged
/// into `corpus_open_failed`, one healthy. Saves an identical file into both.
/// The healthy index is the positive control: waiting for ITS `chunk_count` to
/// grow proves the watcher pipeline, the debounce window, and the OS
/// notification all work in this environment, so a flat `chunk_count` on the
/// quarantined index is a refusal and not merely a slow watcher. Waiting for
/// the quarantined index's refusal counter to move proves the event actually
/// reached the guard.
/// Test: this IS the test. Against pre-fix code the quarantined index's
/// `chunk_count()` grows past 0 exactly as it did in production.
// `multi_thread` is REQUIRED, not stylistic: the watcher bridges its OS
// notification thread into the async loop, and on the default current-thread
// runtime that pipeline can starve — which is how this test passed on macOS
// and failed on Linux CI. Mirrors `watch_loop::tests::modified_file_triggers_indexing`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quarantined_index_refuses_watcher_write_and_chunk_count_stays_zero() {
    let broken_dir = tempdir().expect("tempdir");
    let healthy_dir = tempdir().expect("tempdir");
    let broken_root = broken_dir.path().to_path_buf();
    let healthy_root = healthy_dir.path().to_path_buf();
    let embedder = mock_embedder();

    // Force the corpus open to fail for the first index only.
    sabotage_corpus_path(&broken_root);

    let broken = build_indexer_from_entry(&entry_at("broken-4122", &broken_root), &embedder)
        .await
        .expect("build broken indexer");
    let healthy = build_indexer_from_entry(&entry_at("healthy-4122", &healthy_root), &embedder)
        .await
        .expect("build healthy indexer");

    assert!(
        broken.corpus_open_failed,
        "precondition: a directory at the redb path must fail the corpus open"
    );
    assert!(
        broken.is_write_quarantined(),
        "a corpus-open failure must put the index into write quarantine (#4122)"
    );
    assert!(
        !healthy.corpus_open_failed && !healthy.is_write_quarantined(),
        "precondition: the control index must open its corpus cleanly"
    );
    assert_eq!(
        broken.chunk_count(),
        0,
        "precondition: broken index is empty"
    );

    let broken = Arc::new(RwLock::new(broken));
    let healthy = Arc::new(RwLock::new(healthy));

    let _broken_watch = spawn_watch_loop(
        &broken_root,
        IndexId::new("broken"),
        Arc::clone(&broken),
        IndexedFiles::new(),
        Arc::new(trusty_search::core::file_events::FileEventFeed::new()),
    )
    .expect("spawn broken watch loop");
    let _healthy_watch = spawn_watch_loop(
        &healthy_root,
        IndexId::new("healthy"),
        Arc::clone(&healthy),
        IndexedFiles::new(),
        Arc::new(trusty_search::core::file_events::FileEventFeed::new()),
    )
    .expect("spawn healthy watch loop");

    // Ordinary, unrelated saves in each worktree — the production trigger.
    // Driven by `await_condition_resaving` so a save that lands before the OS
    // watch is armed cannot strand the test (see that helper's docs).
    let roots: [&Path; 2] = [&broken_root, &healthy_root];

    // Positive control: the watcher pipeline demonstrably works here.
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
        "control failed: the healthy index never picked up the save, so this \
         environment cannot distinguish a refusal from a stalled watcher"
    );

    // Wait until the broken index has DECIDED about the event — either it
    // refused (fixed) or it accepted and grew a corpus (the bug). Latching on
    // "decided" rather than on "refused" keeps the headline assertion below
    // the one that fails, so a regression reports the corruption directly
    // instead of a missing counter bump.
    let decided = {
        let broken = Arc::clone(&broken);
        await_condition_resaving(&roots, "probe.rs", move || {
            let broken = Arc::clone(&broken);
            async move {
                let broken = broken.read().await;
                broken.refused_incremental_writes() > 0 || broken.chunk_count() > 0
            }
        })
        .await
    };
    assert!(
        decided,
        "the watcher event never reached the broken index at all — the test \
         cannot distinguish a refusal from a dropped event"
    );

    // THE ASSERTION: no partial corpus was built over the unopened one.
    let broken = broken.read().await;
    assert_eq!(
        broken.chunk_count(),
        0,
        "#4122: a write-quarantined index must not grow a chunk corpus from \
         watcher writes — this is the 0 → 68 → 1334 climb that destroyed \
         duettoresearch-hackathon-tm-hackathon-01"
    );
    assert!(
        broken.refused_incremental_writes() > 0,
        "the refusal must be counted so operators can see how many saves were \
         dropped while the index was quarantined"
    );
    assert!(
        broken.is_write_quarantined(),
        "the index must remain quarantined until a corpus open succeeds"
    );
}

/// Issue #4122 — THE RECOVERY CASE (anti-over-refusal).
///
/// Why: refusing writes forever on one transient failure would trade a data
/// loss bug for an availability bug. The incident's contrast case
/// (`cto-duetto`) failed its open in the same boot and recovered its full
/// 200,090-chunk corpus on the next restart — that path must keep working.
/// This test pins BOTH halves of the boundary: refused while broken, accepted
/// again after a successful reopen, with the original corpus still intact.
/// What: seeds a real redb corpus with the "original" chunks and keeps the
/// handle open, so `build_indexer_from_entry`'s open fails with
/// `DatabaseAlreadyOpen` — a transient failure that leaves the file perfectly
/// readable. Asserts the write is refused and the durable corpus is untouched;
/// then releases the lock, reopens, wires the store, and asserts writes flow
/// again while every original chunk is still present.
/// Test: this IS the test. Against pre-fix code the refusal assertions fail
/// (the write is accepted); a fix that quarantines permanently fails the
/// second half.
#[tokio::test]
async fn successful_reopen_lifts_quarantine_and_leaves_corpus_intact() {
    let tmp = tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    let embedder = mock_embedder();

    // ── The "original" durable corpus, as it existed before the bad boot ──
    let colocated = root.join(".trusty-search");
    std::fs::create_dir_all(&colocated).expect("create colocated dir");
    let redb_path = colocated.join("index.redb");

    let (original_chunks, _) = chunk_ast(
        "src/original.rs",
        "pub fn alpha() -> u32 { 1 }\npub fn beta() -> u32 { 2 }\n",
    );
    assert!(
        !original_chunks.is_empty(),
        "precondition: the fixture must produce at least one original chunk"
    );
    let original_ids: Vec<String> = original_chunks.iter().map(|c| c.id.clone()).collect();

    let live_handle = CorpusStore::open(&redb_path).expect("open original corpus");
    live_handle
        .upsert_chunks(&original_chunks)
        .expect("seed original corpus");
    assert_eq!(
        live_handle.chunk_count().expect("count original corpus"),
        original_ids.len(),
        "precondition: the original corpus must be seeded"
    );

    // ── The bad boot: the open fails because the file is already locked ──
    let mut indexer = build_indexer_from_entry(&entry_at("recovery-4122", &root), &embedder)
        .await
        .expect("build indexer");
    assert!(
        indexer.corpus_open_failed,
        "precondition: a concurrently-held redb must fail the second open \
         (DatabaseAlreadyOpen)"
    );
    assert!(indexer.is_write_quarantined(), "the index must quarantine");

    // ── Half 1: writes are refused while quarantined ──
    let err = indexer
        .index_file("src/unrelated.rs", "pub fn unrelated() {}\n")
        .await
        .expect_err("a quarantined index must refuse an incremental write");
    assert!(
        err.to_string().contains("write-quarantined"),
        "the refusal must say why it refused, got: {err}"
    );
    assert_eq!(
        indexer.chunk_count(),
        0,
        "#4122: no partial corpus may be built while quarantined"
    );
    assert_eq!(
        indexer.refused_incremental_writes(),
        1,
        "the refusal must be counted"
    );
    assert_eq!(
        live_handle.chunk_count().expect("count original corpus"),
        original_ids.len(),
        "the on-disk corpus must be untouched while quarantined"
    );

    // ── The restart: the lock is released and the corpus opens cleanly ──
    drop(live_handle);
    let reopened = CorpusStore::open(&redb_path).expect("reopen corpus after the lock is released");
    indexer.set_corpus_store(Arc::new(reopened));

    assert!(
        !indexer.is_write_quarantined(),
        "#4122: a successful corpus open MUST lift the quarantine — this is \
         the cto-duetto recovery path and it must not be broken by the fix"
    );
    assert_eq!(
        indexer.refused_incremental_writes(),
        0,
        "the refusal counter must reset when the quarantine lifts"
    );

    // ── Half 2: writes flow again, and the original corpus survived ──
    indexer
        .index_file("src/after_recovery.rs", "pub fn recovered() -> u32 { 7 }\n")
        .await
        .expect("writes must be accepted again after a successful reopen");
    assert!(
        indexer.chunk_count() > 0,
        "a recovered index must accept incremental writes again"
    );

    let corpus = indexer.corpus_store().expect("corpus store must be wired");
    let id_refs: Vec<&str> = original_ids.iter().map(String::as_str).collect();
    let survivors = corpus.get_chunks(&id_refs).expect("read original chunks");
    assert_eq!(
        survivors.len(),
        original_ids.len(),
        "every original chunk must survive the quarantine-and-recover cycle \
         intact — the whole point of refusing writes was to keep them"
    );
}

/// Issue #4122 — THE REFUSAL MUST BE OBSERVABLE.
///
/// Why: a silent no-op would reproduce the original sin of this bug class —
/// the system quietly holding a state it does not report. In this workspace
/// `warn!` does NOT persist: `trusty_common::error_capture::BugCaptureLayer`
/// (installed by the daemon via `init_tracing_with_buffer_and_capture`) drops
/// every event that is not `Level::ERROR`, so a WARN refusal would never
/// reach `errors.jsonl`, `list_recent_errors`, or `tm doctor`. This test
/// therefore asserts capture through the REAL layer rather than asserting
/// that some log line exists.
/// What: builds a quarantined indexer FIRST (so the loader's own
/// corpus-open ERROR is not in the store), then installs a scoped subscriber
/// carrying a `BugCaptureLayer` backed by a temp-file `ErrorStore`, issues one
/// refused write, and asserts a captured record naming the index, the issue,
/// and the refusal.
/// Test: this IS the test. Against pre-fix code no refusal happens at all, so
/// no record is captured.
#[tokio::test]
async fn quarantine_refusal_emits_error_level_diagnostic() {
    use tracing_subscriber::layer::SubscriberExt as _;
    use trusty_common::error_capture::{BugCaptureLayer, ErrorStore};

    let tmp = tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    sabotage_corpus_path(&root);

    // Built before the capture layer is installed so the only record under
    // test is the refusal itself, not the loader's corpus-open ERROR.
    let indexer = build_indexer_from_entry(&entry_at("observable-4122", &root), &mock_embedder())
        .await
        .expect("build indexer");
    assert!(indexer.is_write_quarantined(), "precondition: quarantined");

    let store = ErrorStore::with_path(Some(tmp.path().join("errors.jsonl")), 64);
    let layer = BugCaptureLayer::new(store.clone(), env!("CARGO_PKG_VERSION"));
    let subscriber = tracing_subscriber::registry().with(layer);
    let _guard = tracing::subscriber::set_default(subscriber);

    assert!(
        store.is_empty(),
        "precondition: the capture store starts empty"
    );

    indexer
        .index_file("src/refused.rs", "pub fn refused() {}\n")
        .await
        .expect_err("the write must be refused");

    let captured = store.recent_errors(16);
    let refusal = captured
        .iter()
        .find(|e| e.message.contains("#4122"))
        .unwrap_or_else(|| {
            panic!(
                "#4122: the refusal must be emitted at ERROR so it persists to \
                 errors.jsonl / list_recent_errors / tm doctor; captured \
                 records were: {captured:?}"
            )
        });
    assert!(
        refusal.message.contains("REFUSING incremental write"),
        "the captured record must state that a write was refused, got: {}",
        refusal.message
    );
    assert!(
        refusal.message.contains("observable-4122"),
        "the captured record must name the affected index, got: {}",
        refusal.message
    );
    assert!(
        refusal.fields.contains("src/refused.rs"),
        "the captured record must name the refused target, got: {}",
        refusal.fields
    );
}
