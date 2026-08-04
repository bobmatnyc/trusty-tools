//! End-to-end resume-from-checkpoint tests (issue #3979).
//!
//! Why: the pure adopt/discard decision is unit-tested in
//! `checkpoint_tests.rs`, but the claim that actually matters is behavioural —
//! *a reindex that is interrupted and then resumed produces the same index a
//! clean full reindex would have produced*. That can only be shown by driving
//! the real pipeline twice and comparing the promoted corpora, which is what
//! `interrupted_reindex_resumes_to_identical_index` does.
//!
//! What: a colocated-storage harness (`.trusty-search/` under a tempdir) so
//! every path — live corpus, staging corpus, HNSW snapshot — resolves inside
//! the test's own directory rather than the daemon's global data dir. That
//! keeps these tests hermetic and safe to run in parallel with the rest of the
//! suite, unlike a `TRUSTY_DATA_DIR` override.
//!
//! The interruption is modelled the way a real crash leaves the disk: a
//! staging corpus at `index.redb.tmp` holding the carryover rows plus the
//! batches the interrupted run had already committed, carrying a checkpoint
//! record — all of it produced by the real pipeline, not hand-built chunk
//! literals, so the test cannot drift away from what the pipeline actually
//! writes.
//!
//! Test: this IS the test module.

use super::*;
use crate::core::corpus::CorpusStore;
use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexId, IndexStages};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;

/// Create a project root with colocated storage and the given files.
///
/// Why: every path the reindex writes to is derived from the root when
/// `.trusty-search/` exists, so creating that dir first is what makes the test
/// hermetic.
/// What: makes the tempdir, the `.trusty-search/` dir, and each `(name,
/// contents)` file.
fn make_root(files: &[(&str, &str)]) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(tmp.path().join(".trusty-search")).expect("colocated dir");
    for (name, contents) in files {
        let path = tmp.path().join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("parent dir");
        }
        std::fs::write(&path, contents).expect("write fixture");
    }
    tmp
}

/// Build an index handle backed by the colocated live corpus under `root`.
///
/// Why: staging (and therefore resume) only engages when the index has a
/// durable corpus store — `staging::should_stage(has_corpus_store())`.
/// What: opens `<root>/.trusty-search/index.redb`, wires it onto a
/// `CodeIndexer` with no embedder (so the run takes the `parse_files_only`
/// path and stays hermetic), and returns the handle. `defer_embed` is `true`,
/// matching the daemon's default for a freshly registered index.
fn make_handle(root: &Path, id: &str) -> Arc<IndexHandle> {
    let db_path = crate::service::colocated_storage::colocated_redb_path(root).expect("redb path");
    let corpus = CorpusStore::open(&db_path).expect("open live corpus");
    let mut indexer = CodeIndexer::new(id, root.to_path_buf());
    indexer.set_corpus_store(Arc::new(corpus));
    let mut skip_dirs = crate::service::walker::default_extra_skip_dirs();
    // Never walk our own storage dir — a redb file is not source.
    skip_dirs.push(".trusty-search".to_string());
    Arc::new(IndexHandle {
        id: IndexId::new(id),
        indexer: Arc::new(tokio::sync::RwLock::new(indexer)),
        root_path: root.to_path_buf(),
        include_paths: vec![],
        exclude_globs: vec![],
        extensions: vec![],
        domain_terms: vec![],
        include_docs: false,
        respect_gitignore: true,
        follow_links: true,
        extra_skip_dirs: skip_dirs,
        data_file_max_bytes: crate::service::walker::DEFAULT_DATA_FILE_MAX_BYTES,
        path_filter: vec![],
        context_embedding: Arc::new(tokio::sync::RwLock::new(None)),
        context_summary: Arc::new(tokio::sync::RwLock::new(None)),
        indexed_head_sha: Arc::new(tokio::sync::RwLock::new(None)),
        last_indexed_at: Arc::new(tokio::sync::RwLock::new(None)),
        lexical_only: false,
        skip_kg: false,
        skip_vector: false,
        defer_embed: true,
        stages: Arc::new(tokio::sync::RwLock::new(IndexStages::default())),
        search_pressure: Arc::new(tokio::sync::Notify::new()),
        walk_diagnostics: Arc::new(tokio::sync::RwLock::new(
            crate::core::registry::WalkDiagnostics::default(),
        )),
    })
}

/// Run one non-force reindex to completion and return its progress handle.
async fn reindex(handle: &Arc<IndexHandle>) -> Arc<ReindexProgress> {
    let progress = Arc::new(ReindexProgress::new());
    spawn_reindex_awaitable(handle.clone(), progress.clone(), false)
        .await
        .expect("reindex task must not panic");
    assert_eq!(
        progress.status.load(),
        ReindexStatus::Complete,
        "reindex must complete"
    );
    progress
}

/// Canonical, comparable rendering of a promoted corpus's durable content.
///
/// Why: "the resumed index equals a clean full reindex" has to be an exact
/// structural comparison, not a chunk count. Serializing the id-sorted chunk
/// set plus the file-hash table to JSON gives a single string that differs if
/// ANY field of ANY chunk differs — content, line range, language, chunk type,
/// call edges, parent/child ids, virtual terms.
/// What: reads through the corpus store the handle currently holds — i.e. the
/// promoted `index.redb` the daemon would serve — rather than opening a second
/// handle on the same path, which redb refuses (single writer per file).
async fn corpus_fingerprint(handle: &IndexHandle) -> String {
    let store = handle
        .indexer
        .read()
        .await
        .corpus_store()
        .expect("index must hold a durable corpus after a staged reindex");
    let mut chunks = store.load_all_chunks().expect("load chunks");
    chunks.sort_by(|a, b| a.id.cmp(&b.id));
    let mut hashes = store.load_file_hashes().expect("load hashes");
    hashes.sort();
    serde_json::to_string(&(chunks, hashes)).expect("serialize corpus fingerprint")
}

/// Copy a promoted corpus into the staging slot and stamp it with `checkpoint`,
/// reproducing the on-disk state a crash mid-reindex leaves behind.
///
/// Why: a real SIGKILL cannot be issued against an in-process reindex, but the
/// state it leaves is fully specified — an `index.redb.tmp` containing the
/// carryover rows plus every batch that committed before the crash, carrying
/// the checkpoint record written when staging began. Producing that file by
/// copying a REAL promoted corpus (rather than hand-writing chunk literals)
/// means the fixture always matches what the pipeline actually emits.
/// What: byte-copies `live` to the colocated staging path under `root` and
/// writes `checkpoint_json` into the copy's `_meta` table. Passing malformed
/// bytes is how the corrupt-checkpoint test is built.
fn plant_staging_corpus(root: &Path, live: &Path, checkpoint_json: &[u8]) -> PathBuf {
    let tmp_path =
        crate::service::colocated_storage::colocated_redb_tmp_path(root).expect("tmp path");
    std::fs::copy(live, &tmp_path).expect("copy live corpus into the staging slot");
    let staged = CorpusStore::open(&tmp_path).expect("open planted staging corpus");
    staged
        .write_reindex_checkpoint_sync(checkpoint_json)
        .expect("stamp checkpoint");
    drop(staged);
    tmp_path
}

/// The checkpoint a genuine interrupted run would have written for `handle`.
///
/// Note the root is CANONICALIZED exactly as `runner::run_reindex` does before
/// stamping the record — on macOS a `/var/folders/...` tempdir canonicalizes to
/// `/private/var/folders/...`, and a checkpoint recorded under the uncanonical
/// form is (correctly) rejected as a root mismatch.
fn checkpoint_for(handle: &IndexHandle, id: &str, root: &Path) -> checkpoint::ReindexCheckpoint {
    let canonical = super::validate::canonical_walk_root(root);
    checkpoint::ReindexCheckpoint::for_run(handle, &IndexId::new(id), &canonical)
}

/// `checkpoint_for` serialized to the JSON bytes stored in `_meta`.
fn valid_checkpoint_json(handle: &IndexHandle, id: &str, root: &Path) -> Vec<u8> {
    serde_json::to_vec(&checkpoint_for(handle, id, root)).expect("serialize checkpoint")
}

/// The four-file corpus every test in this module indexes.
const FIXTURE: &[(&str, &str)] = &[
    ("a.rs", "pub fn alpha() -> u32 { 1 }\n"),
    ("b.rs", "pub fn bravo() -> u32 { 2 }\n"),
    ("c.rs", "pub fn charlie() -> u32 { 3 }\n"),
    ("d.rs", "pub fn delta() -> u32 { 4 }\n"),
];

/// Build the reference fingerprint: a clean, uninterrupted full reindex of the
/// whole fixture in its own root.
///
/// `id` must be unique per caller: the content-hash cache
/// (`hash::file_hashes()`) is a PROCESS-GLOBAL map keyed by index id, and its
/// inner keys are root-RELATIVE paths. Two reference runs sharing an id would
/// see each other's hashes, skip every file, and promote an empty corpus.
async fn clean_full_reindex_fingerprint(id: &str) -> String {
    let root = make_root(FIXTURE);
    let handle = make_handle(root.path(), id);
    reindex(&handle).await;
    let fp = corpus_fingerprint(&handle).await;
    // Drop the handle (and its redb lock) before the tempdir is removed.
    drop(handle);
    drop(root);
    fp
}

/// THE core guarantee of issue #3979: an interrupted reindex that resumes must
/// produce exactly the index a clean full reindex would have produced, while
/// actually skipping the work the interrupted run had already done.
///
/// Why: resume is only worth having if it is both *faster* (the pre-crash files
/// are skipped, not re-embedded) and *identical* (the promoted corpus matches a
/// clean rebuild field-for-field). A change that satisfies one without the other
/// is a regression: skipping nothing makes the feature pointless, and skipping
/// wrongly silently corrupts the index. This test asserts both.
///
/// What:
///   1. Reindex a reference root holding all four files → reference fingerprint.
///   2. In a second root, index only `a.rs`/`b.rs`, then copy that promoted
///      corpus into `index.redb.tmp` and stamp a valid checkpoint — the exact
///      on-disk state a crash leaves after committing two of four files.
///   3. Create `c.rs`/`d.rs` and reindex. The probe adopts the staging corpus.
///   4. Assert the `start` event reports `resumed: true`, that `a.rs`/`b.rs`
///      were skipped rather than re-indexed, and that the promoted corpus
///      fingerprint is byte-identical to the reference.
///
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn interrupted_reindex_resumes_to_identical_index() {
    let expected = clean_full_reindex_fingerprint("resume-reference-identical").await;

    // ── The interrupted run: a.rs and b.rs committed, c.rs/d.rs not yet ──
    let root = make_root(&FIXTURE[..2]);
    let handle = make_handle(root.path(), "resume-subject");
    reindex(&handle).await;
    let live =
        crate::service::colocated_storage::colocated_redb_path(root.path()).expect("live path");
    let checkpoint = valid_checkpoint_json(&handle, "resume-subject", root.path());
    let tmp_path = plant_staging_corpus(root.path(), &live, &checkpoint);
    assert!(tmp_path.exists(), "staging corpus fixture must exist");

    // The remaining files appear on disk, as they were all along in the real
    // interrupted run — only their batches had not committed yet.
    for (name, contents) in &FIXTURE[2..] {
        std::fs::write(root.path().join(name), contents).expect("write remaining fixture");
    }

    // ── The resumed run ────────────────────────────────────────────────────
    let progress = reindex(&handle).await;

    let events = progress.events.lock().await.clone();
    let start = events
        .iter()
        .map(|e| serde_json::from_str::<serde_json::Value>(e).expect("event json"))
        .find(|e| e["event"] == "start")
        .expect("a start event must be emitted");
    assert_eq!(
        start["resumed"], true,
        "#3979: the run must report that it adopted a checkpoint; got {start}"
    );

    // Resume must actually save work: a.rs and b.rs are hash-matched against
    // the STAGED hash table and skipped. Without resume the adopted done-set
    // would be empty and all four files would be re-indexed.
    assert_eq!(
        progress.total_files.load(Ordering::Acquire),
        4,
        "all four files must still be walked — resume never shortens the walk"
    );
    assert_eq!(
        progress.skipped.load(Ordering::Acquire),
        2,
        "#3979: the two files the interrupted run committed must be skipped"
    );

    assert_eq!(
        corpus_fingerprint(&handle).await,
        expected,
        "#3979: a resumed reindex must produce an index identical to a clean \
         full reindex of the same corpus"
    );
}

/// A resumed run must re-index — never skip — a file whose bytes changed after
/// the interruption.
///
/// Why: this is the correctness property the whole design rests on. The done-set
/// inherited from the staging corpus is a set of *content hashes*, not a set of
/// filenames, and the batch loop re-hashes the file's current bytes before
/// skipping it. If a future change ever made the skip filename-based, a file
/// edited between the crash and the resume would keep its stale chunks forever —
/// a silent, permanent wrong answer. This test fails loudly if that happens.
///
/// What: same interruption fixture, but `a.rs` is rewritten before the resumed
/// run. Asserts the promoted corpus holds the NEW content and none of the old.
///
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn resume_reindexes_a_file_changed_after_the_interruption() {
    let root = make_root(&FIXTURE[..2]);
    let handle = make_handle(root.path(), "resume-changed");
    reindex(&handle).await;
    let live =
        crate::service::colocated_storage::colocated_redb_path(root.path()).expect("live path");
    let checkpoint = valid_checkpoint_json(&handle, "resume-changed", root.path());
    plant_staging_corpus(root.path(), &live, &checkpoint);

    // `a.rs` changes underneath the checkpoint.
    std::fs::write(
        root.path().join("a.rs"),
        "pub fn alpha_v2() -> u32 { 42 }\n",
    )
    .expect("rewrite a.rs");

    let progress = reindex(&handle).await;
    assert_eq!(
        progress.skipped.load(Ordering::Acquire),
        1,
        "#3979: only the UNCHANGED file (b.rs) may be skipped"
    );

    let store = handle
        .indexer
        .read()
        .await
        .corpus_store()
        .expect("durable corpus");
    let chunks = store.load_all_chunks().expect("load chunks");
    let a_content: String = chunks
        .iter()
        .filter(|c| c.file == "a.rs")
        .map(|c| c.content.clone())
        .collect();
    assert!(
        a_content.contains("alpha_v2"),
        "#3979: the edited file must be re-indexed with its NEW content; got {a_content:?}"
    );
    assert!(
        !a_content.contains("pub fn alpha()"),
        "#3979: the pre-interruption content must not survive; got {a_content:?}"
    );
}

/// A corrupt checkpoint record must degrade to a full reindex — never to a
/// partial or wrong index.
///
/// Why: the checkpoint is read from a file that a crash was, by definition,
/// writing to. Garbage bytes must be indistinguishable from "no checkpoint" in
/// their effect: discard the staging corpus, rebuild everything, leave the live
/// corpus intact. Anything else turns an unreadable byte range into an index
/// defect.
/// What: plants a staging corpus whose checkpoint blob is not valid JSON, runs
/// the reindex, and asserts the result matches a clean full reindex and that the
/// unadoptable staging file was removed.
///
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn corrupt_checkpoint_falls_back_to_full_reindex() {
    let expected = clean_full_reindex_fingerprint("resume-reference-corrupt").await;

    let root = make_root(&FIXTURE[..2]);
    let handle = make_handle(root.path(), "resume-corrupt");
    reindex(&handle).await;
    let live =
        crate::service::colocated_storage::colocated_redb_path(root.path()).expect("live path");
    let tmp_path = plant_staging_corpus(root.path(), &live, b"{not valid json at all");
    assert!(tmp_path.exists());

    for (name, contents) in &FIXTURE[2..] {
        std::fs::write(root.path().join(name), contents).expect("write remaining fixture");
    }

    let progress = reindex(&handle).await;
    let events = progress.events.lock().await.clone();
    let start = events
        .iter()
        .map(|e| serde_json::from_str::<serde_json::Value>(e).expect("event json"))
        .find(|e| e["event"] == "start")
        .expect("a start event must be emitted");
    assert_eq!(
        start["resumed"], false,
        "#3979: a corrupt checkpoint must NOT be adopted; got {start}"
    );
    assert_eq!(
        corpus_fingerprint(&handle).await,
        expected,
        "#3979: the fallback must still produce a complete, correct index"
    );
}

/// A checkpoint whose recorded root does not match the current walk root must
/// be discarded.
///
/// Why: staged chunk paths are stored RELATIVE to the walk root (#402), so
/// adopting a checkpoint written against a different root would splice paths
/// that resolve into the wrong tree — the #2178 root-hijack failure class. This
/// is the stale-checkpoint case: the record parses perfectly, and is rejected on
/// content rather than on syntax.
/// What: plants a syntactically valid checkpoint carrying a foreign
/// `canonical_root`, and asserts the run does not resume and still produces a
/// complete index identical to a clean rebuild.
///
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn stale_root_checkpoint_falls_back_to_full_reindex() {
    let expected = clean_full_reindex_fingerprint("resume-reference-stale").await;

    let root = make_root(&FIXTURE[..2]);
    let handle = make_handle(root.path(), "resume-stale-root");
    reindex(&handle).await;
    let live =
        crate::service::colocated_storage::colocated_redb_path(root.path()).expect("live path");

    let mut cp = super::checkpoint::ReindexCheckpoint::for_run(
        &handle,
        &IndexId::new("resume-stale-root"),
        root.path(),
    );
    cp.canonical_root = "/somewhere/else/entirely".to_string();
    let json = serde_json::to_vec(&cp).expect("serialize checkpoint");
    plant_staging_corpus(root.path(), &live, &json);

    for (name, contents) in &FIXTURE[2..] {
        std::fs::write(root.path().join(name), contents).expect("write remaining fixture");
    }

    let progress = reindex(&handle).await;
    let events = progress.events.lock().await.clone();
    let start = events
        .iter()
        .map(|e| serde_json::from_str::<serde_json::Value>(e).expect("event json"))
        .find(|e| e["event"] == "start")
        .expect("a start event must be emitted");
    assert_eq!(
        start["resumed"], false,
        "#3979: a checkpoint from a different root must NOT be adopted; got {start}"
    );
    assert_eq!(
        corpus_fingerprint(&handle).await,
        expected,
        "#3979: the fallback must still produce a complete, correct index"
    );
}

/// A completed reindex must leave no checkpoint record in the promoted corpus.
///
/// Why: promotion is a rename, so the staging corpus's `_meta` table becomes
/// live metadata. A finished corpus advertising "reindex in progress" would be a
/// corpus lying about itself, and nothing else would ever clear it — the probe
/// only ever reads `*.tmp` paths.
/// What: runs a normal reindex and asserts the promoted `index.redb` has no
/// checkpoint blob, while confirming a checkpoint really was written during the
/// run (otherwise this test would pass vacuously).
///
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn completed_reindex_leaves_no_checkpoint() {
    let root = make_root(FIXTURE);
    let handle = make_handle(root.path(), "resume-cleared");
    reindex(&handle).await;

    let store = handle
        .indexer
        .read()
        .await
        .corpus_store()
        .expect("durable corpus");
    assert!(
        store
            .read_reindex_checkpoint_sync()
            .expect("read checkpoint")
            .is_none(),
        "#3979: a promoted corpus must not carry an in-progress checkpoint"
    );
    // Guard against a vacuous pass: the run must genuinely have staged and
    // therefore genuinely have had a checkpoint to clear.
    assert!(
        store.chunk_count().expect("chunk count") > 0,
        "the reindex must have staged and promoted a populated corpus"
    );
}

/// With `TRUSTY_REINDEX_RESUME=0`, an otherwise-adoptable checkpoint must be
/// ignored.
///
/// Why: the kill switch is the operator's escape hatch back to pre-#3979
/// behaviour without a downgrade. It has to be verified against the real
/// pipeline, not just against `resume_enabled()`, because a probe that read the
/// flag too late would still have adopted the corpus.
///
/// #4721: `TRUSTY_REINDEX_RESUME` is supplied at process spawn instead of by
/// `unsafe { set_var }` under `#[serial]`. `#[serial]` only excludes other
/// `#[serial]` tests — the binary's ~1400 non-serial tests kept running inside
/// the window, several of which drive reindexes that read this exact variable,
/// so the old form both raced them and (in Rust 2024 terms) raced `getenv`. See
/// `super::test_isolation` and #4213.
/// What: plants a valid checkpoint, and — in a child process started with
/// `TRUSTY_REINDEX_RESUME=0` — asserts the run reports `resumed: false`.
///
/// Test: this test.
#[tokio::test]
async fn resume_kill_switch_disables_adoption() {
    if !super::test_isolation::run_isolated(
        "service::reindex::resume_tests::resume_kill_switch_disables_adoption",
        &[("TRUSTY_REINDEX_RESUME", "0")],
    ) {
        return;
    }
    let root = make_root(&FIXTURE[..2]);
    let handle = make_handle(root.path(), "resume-killswitch");
    reindex(&handle).await;
    let live =
        crate::service::colocated_storage::colocated_redb_path(root.path()).expect("live path");
    let checkpoint = valid_checkpoint_json(&handle, "resume-killswitch", root.path());
    plant_staging_corpus(root.path(), &live, &checkpoint);

    // Guard against a vacuous pass: the child process must genuinely have the
    // kill switch set, or "did not resume" proves nothing.
    assert_eq!(
        std::env::var("TRUSTY_REINDEX_RESUME").ok().as_deref(),
        Some("0"),
        "#4721: this body must only ever run in the isolated child process that \
         was spawned with TRUSTY_REINDEX_RESUME=0"
    );
    let progress = reindex(&handle).await;

    let events = progress.events.lock().await.clone();
    let start = events
        .iter()
        .map(|e| serde_json::from_str::<serde_json::Value>(e).expect("event json"))
        .find(|e| e["event"] == "start")
        .expect("a start event must be emitted");
    assert_eq!(
        start["resumed"], false,
        "#3979: TRUSTY_REINDEX_RESUME=0 must suppress adoption; got {start}"
    );
}

/// The probe must hand its OPEN staging-corpus handle to the adoption, not drop
/// it and let the adoption re-open the same redb file (#4721).
///
/// Why: the shipped code opened `index.redb.tmp` twice — once in
/// `checkpoint::probe_resume` to validate the record, once again in
/// `corpus_swap::adopt_staged_corpus` to install it. redb takes an EXCLUSIVE
/// lock per file, so the two opens are mutually exclusive by construction: they
/// only worked because the first handle happened to be dropped inside the
/// probe's blocking task before the second ran. Between them the staging corpus
/// was owned by nobody, and the second open has real failure modes (a wedged
/// #3659 open gate, a file removed by another daemon, a lock not yet released) —
/// each of which `adopt_staged_corpus` swallowed into `Ok(None)`, silently
/// abandoning the resume and writing to the LIVE corpus instead. The fix makes
/// the file open exactly once, and this test pins it: ownership is observable,
/// because a second open of a held redb file is refused.
///
/// What: probes a planted, adoptable staging corpus, then asserts a competing
/// `CorpusStore::open` on that path is REFUSED while the returned state is
/// alive, and succeeds once the state is dropped — which is what proves the
/// refusal was ownership rather than a corrupt file.
///
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn probe_hands_the_open_staging_corpus_to_the_adoption() {
    let root = make_root(&FIXTURE[..2]);
    let handle = make_handle(root.path(), "resume-single-open");
    reindex(&handle).await;
    let live =
        crate::service::colocated_storage::colocated_redb_path(root.path()).expect("live path");
    let cp = checkpoint_for(&handle, "resume-single-open", root.path());
    let tmp_path = plant_staging_corpus(
        root.path(),
        &live,
        &serde_json::to_vec(&cp).expect("serialize checkpoint"),
    );

    let canonical = super::validate::canonical_walk_root(root.path());
    let state = super::checkpoint::probe_resume(
        &handle,
        &IndexId::new("resume-single-open"),
        &canonical,
        &cp,
    )
    .await
    .expect("the planted checkpoint must be adoptable");

    assert!(
        CorpusStore::open(&tmp_path).is_err(),
        "#4721: the probe must still OWN the staging corpus it validated — a \
         second open succeeding means the handle was released and the adoption \
         has to re-open the file, which is exactly the window this fix closes"
    );

    drop(state);
    CorpusStore::open(&tmp_path).expect(
        "once the resume state is dropped the file must open normally — \
                 the refusal above was ownership, not corruption",
    );
}

/// A promoted corpus must never carry an in-progress checkpoint, even if the
/// staging handle was already released when the promotion started (#4721).
///
/// Why: this invariant used to hold only because `commit_staged_corpus_swap`
/// happened to call the checkpoint clear before `take_corpus_store`. The clear
/// reads the INSTALLED store, so with no store installed it returned early and
/// did nothing — and the rename then promoted a finished corpus advertising
/// itself as mid-reindex, which nothing ever cleans up (the probe only reads
/// `*.tmp` paths). That is one statement swap away in a function no test would
/// have flagged, and it is also reachable today whenever the store is absent at
/// promote time (a #4122 write quarantine, a failed re-install). Encoding the
/// order in `PromotionRelease` plus a fallback clear on the released file makes
/// the invariant hold regardless of how the caller got here.
///
/// What: plants a staging corpus carrying a checkpoint, takes the corpus store
/// out of the indexer FIRST (the exact state a reordered caller would produce),
/// runs the promotion, and asserts the promoted live corpus has no checkpoint
/// while still holding the staged chunks.
///
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn promotion_clears_the_checkpoint_even_when_the_store_was_released_early() {
    let root = make_root(&FIXTURE[..2]);
    let handle = make_handle(root.path(), "resume-promote-order");
    reindex(&handle).await;
    let live =
        crate::service::colocated_storage::colocated_redb_path(root.path()).expect("live path");
    let checkpoint = valid_checkpoint_json(&handle, "resume-promote-order", root.path());
    let tmp_path = plant_staging_corpus(root.path(), &live, &checkpoint);

    // The reordered / quarantined shape: no corpus store installed when the
    // promotion begins, so the clear cannot go through the indexer.
    let _released = handle.indexer.write().await.take_corpus_store();
    drop(_released);

    super::corpus_swap::commit_staged_corpus_swap(
        &handle,
        &IndexId::new("resume-promote-order"),
        &tmp_path,
    )
    .await;

    let promoted = handle
        .indexer
        .read()
        .await
        .corpus_store()
        .expect("the promoted corpus must be installed");
    assert!(
        promoted
            .read_reindex_checkpoint_sync()
            .expect("read checkpoint")
            .is_none(),
        "#4721: a promoted corpus must not carry an in-progress checkpoint, no \
         matter whether the staging handle was still installed when the \
         promotion started"
    );
    // Guard against a vacuous pass: the promotion must really have happened.
    assert!(
        promoted.chunk_count().expect("chunk count") > 0,
        "the staged corpus must actually have been promoted"
    );
}
