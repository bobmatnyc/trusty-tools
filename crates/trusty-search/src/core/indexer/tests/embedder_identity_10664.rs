//! #10664: the embedder-identity stamp is written on build and compared at load.
//!
//! Why: an index can hold vectors from another embedder (INT8 vs fp32
//! MiniLM, both 384-dim) with nothing to detect it.
//! What: a real reindex stamps the corpus; a reload through
//! `build_indexer_from_entry` compares the stamp with the running embedder,
//! WARNs on a mismatch or an unreadable stamp, and still returns the indexer.
//! Test: `a_completed_reindex_stamps_the_embedder_identity`,
//! `an_unstamped_index_reports_unknown_and_still_loads`,
//! `a_mismatched_stamp_is_reported_and_the_index_still_loads`,
//! `a_matching_stamp_reports_nothing`,
//! `an_unreadable_stamp_reports_unreadable_not_match`,
//! `the_status_body_carries_the_identity_check`,
//! `switchable_reports_the_active_backend_identity`,
//! `compare_covers_every_outcome`, `should_stamp_only_full_rebuilds`.

use std::sync::Arc;

use crate::core::corpus::CorpusStore;
use crate::core::embed::{Embedder, MockEmbedder};
use crate::core::embedder_identity::{should_stamp, EmbedderIdentity, IdentityCheck, StampRead};
use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexId};
use crate::core::store::UsearchStore;
use crate::service::embedder_supervisor::{
    ActiveBackend, BackendKind, BootstrapState, SwitchableEmbedder,
};
use crate::service::persistence::PersistedIndex;
use crate::service::persistence_loader::build_indexer_from_entry;
use crate::service::reindex::{spawn_reindex_awaitable, ReindexProgress, ReindexStatus};
use trusty_common::embedder::{ExecutionProvider, EMBED_DIM};

const MODEL: &str = "all-MiniLM-L6-v2";

/// The daemon's installed embedder shape: a `SwitchableEmbedder` over a mock.
fn switchable(quantized: bool) -> Arc<dyn Embedder> {
    let active = ActiveBackend {
        kind: BackendKind::Ort,
        provider: ExecutionProvider::Cpu,
        model: MODEL.to_string(),
        quantized,
        bootstrap: BootstrapState::NotApplicable,
    };
    Arc::new(SwitchableEmbedder::new(
        Arc::new(MockEmbedder::new(EMBED_DIM)),
        active,
    ))
}

fn fp32() -> EmbedderIdentity {
    EmbedderIdentity::new(MODEL, EMBED_DIM, false)
}

fn int8() -> EmbedderIdentity {
    EmbedderIdentity::new(MODEL, EMBED_DIM, true)
}

fn entry(id: &str, root: &std::path::Path) -> PersistedIndex {
    PersistedIndex {
        id: id.to_string(),
        root_path: root.to_path_buf(),
        colocated: true,
        ..Default::default()
    }
}

/// Build a colocated index once so its corpus exists, run `plant` against
/// that corpus, then drop it so the reload opens the file again.
async fn seed(id: &str, root: &std::path::Path, plant: impl FnOnce(&CorpusStore)) {
    let indexer = build_indexer_from_entry(&entry(id, root), &switchable(false))
        .await
        .expect("first build");
    plant(&indexer.corpus_store().expect("corpus"));
}

/// WARN lines written on this thread while the guard lives.
#[derive(Clone, Default)]
struct Capture(Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("capture lock").extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Capture {
    fn identity_warnings(&self) -> Vec<String> {
        let bytes = self.0.lock().expect("capture lock").clone();
        String::from_utf8_lossy(&bytes)
            .lines()
            .filter(|l| l.contains("embedder identity"))
            .map(str::to_owned)
            .collect()
    }
}

fn capture() -> (Capture, tracing::subscriber::DefaultGuard) {
    let cap = Capture::default();
    let writer = cap.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_max_level(tracing::Level::WARN)
        .with_ansi(false)
        .without_time()
        .finish();
    (cap, tracing::subscriber::set_default(subscriber))
}

/// Reload the seeded index under log capture; return it and its WARN lines.
async fn reload(id: &str, root: &std::path::Path, quantized: bool) -> (CodeIndexer, Vec<String>) {
    let (cap, guard) = capture();
    let indexer = build_indexer_from_entry(&entry(id, root), &switchable(quantized))
        .await
        .expect("a load never fails on the identity check");
    drop(guard);
    (indexer, cap.identity_warnings())
}

/// Why: #10664 criterion (1) — a build records which embedder made its vectors.
/// What: a fresh index's first reindex stamps fp32; an incremental reindex
/// over a planted int8 stamp keeps it, so a mixed store is not relabelled;
/// a forced reindex replaces every vector and re-stamps fp32.
/// Test: this test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::parallel]
async fn a_completed_reindex_stamps_the_embedder_identity() {
    let root = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(root.path().join(".trusty-search")).expect("store dir");
    std::fs::write(root.path().join("a.rs"), "pub fn alpha() {}\n").expect("source");
    let corpus = CorpusStore::open(&root.path().join(".trusty-search/index.redb")).expect("open");
    let mut indexer = CodeIndexer::new("ident-10664", root.path().to_path_buf())
        .with_storage_layout(crate::service::storage_layout::StorageLayout::Colocated)
        .with_components(
            switchable(false),
            Arc::new(UsearchStore::new(EMBED_DIM).expect("usearch")),
        );
    indexer.set_corpus_store(Arc::new(corpus));
    let mut handle = IndexHandle::bare(
        IndexId::new("ident-10664"),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        root.path().to_path_buf(),
    );
    handle.defer_embed = false;
    handle.extra_skip_dirs.push(".trusty-search".into());
    let handle = Arc::new(handle);
    let stamp = |h: Arc<IndexHandle>| async move {
        let corpus = h.indexer.read().await.corpus_store().expect("corpus");
        corpus.read_embedder_identity_sync()
    };

    for (run, plant_int8, force, expected) in [
        ("fresh", false, false, fp32()),
        ("incremental", true, false, int8()),
        ("force", true, true, fp32()),
    ] {
        if plant_int8 {
            let corpus = handle.indexer.read().await.corpus_store().expect("corpus");
            corpus.write_embedder_identity_sync(&int8()).expect("plant");
        }
        let progress = Arc::new(ReindexProgress::new());
        spawn_reindex_awaitable(handle.clone(), progress.clone(), force)
            .await
            .expect("reindex task");
        assert_eq!(
            progress.status.load(),
            ReindexStatus::Complete,
            "{run} reindex completes"
        );
        assert_eq!(
            stamp(handle.clone()).await,
            StampRead::Stamped(expected),
            "{run} reindex leaves the expected stamp"
        );
    }
}

/// Why: an index written before #10664 must load exactly as before.
/// What: no stamp → `unknown`, no WARN, and the indexer is returned.
/// Test: this test.
#[tokio::test]
#[serial_test::parallel]
async fn an_unstamped_index_reports_unknown_and_still_loads() {
    let root = tempfile::tempdir().expect("tempdir");
    seed("unstamped-10664", root.path(), |_| {}).await;
    let (indexer, warnings) = reload("unstamped-10664", root.path(), false).await;
    assert!(
        indexer.has_corpus_store(),
        "the index loads with its corpus"
    );
    assert_eq!(
        indexer.embedder_identity_check().await,
        IdentityCheck::Unknown
    );
    assert!(
        warnings.is_empty(),
        "unknown is not a mismatch: {warnings:?}"
    );
}

/// Why: #10664 criterion (2) — a load reports a store whose stamp differs.
/// What: an int8 stamp loaded under an fp32 embedder yields one WARN naming
/// both identities, a `mismatch` check, and an indexer that still loads.
/// Test: this test.
#[tokio::test]
#[serial_test::parallel]
async fn a_mismatched_stamp_is_reported_and_the_index_still_loads() {
    let root = tempfile::tempdir().expect("tempdir");
    seed("mismatch-10664", root.path(), |c| {
        c.write_embedder_identity_sync(&int8()).expect("plant int8");
    })
    .await;
    let (indexer, warnings) = reload("mismatch-10664", root.path(), false).await;
    assert!(indexer.has_corpus_store(), "the index still loads");
    assert_eq!(
        indexer.embedder_identity_check().await,
        IdentityCheck::Mismatch {
            stored: int8(),
            active: fp32()
        }
    );
    assert_eq!(warnings.len(), 1, "exactly one WARN: {warnings:?}");
    assert!(
        warnings[0].contains(&int8().to_string()) && warnings[0].contains(&fp32().to_string()),
        "the WARN names both identities: {}",
        warnings[0]
    );
}

/// Why: a correct index must not produce noise.
/// What: an fp32 stamp loaded under fp32 → `match`, no WARN.
/// Test: this test.
#[tokio::test]
#[serial_test::parallel]
async fn a_matching_stamp_reports_nothing() {
    let root = tempfile::tempdir().expect("tempdir");
    seed("match-10664", root.path(), |c| {
        c.write_embedder_identity_sync(&fp32()).expect("plant fp32");
    })
    .await;
    let (indexer, warnings) = reload("match-10664", root.path(), false).await;
    assert_eq!(
        indexer.embedder_identity_check().await,
        IdentityCheck::Match(fp32())
    );
    assert!(warnings.is_empty(), "a match logs nothing: {warnings:?}");
}

/// Why: Fail-Open Check — a stamp that cannot be decoded must not read as a match.
/// What: undecodable bytes → `unreadable`, one WARN, and the index loads.
/// Test: this test.
#[tokio::test]
#[serial_test::parallel]
async fn an_unreadable_stamp_reports_unreadable_not_match() {
    let root = tempfile::tempdir().expect("tempdir");
    seed("unreadable-10664", root.path(), |c| {
        c.write_embedder_identity_bytes(b"{not json")
            .expect("plant garbage");
    })
    .await;
    let (indexer, warnings) = reload("unreadable-10664", root.path(), false).await;
    assert!(indexer.has_corpus_store(), "the index still loads");
    let check = indexer.embedder_identity_check().await;
    assert_eq!(check.status(), "unreadable", "got {check:?}");
    assert_eq!(warnings.len(), 1, "one WARN: {warnings:?}");
}

/// Why: the status output is where an operator reads the check.
/// What: the JSON carries status, both identities, and the reason field.
/// Test: this test.
#[test]
fn the_status_body_carries_the_identity_check() {
    let check = IdentityCheck::compare(StampRead::Stamped(int8()), Some(fp32()));
    let body = check.to_json(Some(&fp32()));
    assert_eq!(body["status"], "mismatch");
    assert_eq!(body["stored"]["precision"], "int8");
    assert_eq!(body["active"]["precision"], "fp32");
    assert!(body["reason"].is_null());
    let unknown = IdentityCheck::compare(StampRead::Absent, Some(fp32())).to_json(Some(&fp32()));
    assert_eq!(unknown["status"], "unknown");
    assert!(unknown["stored"].is_null());
}

/// Why: the daemon's installed embedder is the one whose identity is stamped.
/// What: `SwitchableEmbedder` reports its `ActiveBackend` model and precision.
/// Test: this test.
#[test]
fn switchable_reports_the_active_backend_identity() {
    assert_eq!(switchable(true).identity(), Some(int8()));
    assert_eq!(switchable(false).identity(), Some(fp32()));
    assert_eq!(MockEmbedder::new(EMBED_DIM).identity(), None);
}

/// Why: the comparison is exact; no outcome may collapse into another.
/// Test: this test.
#[test]
fn compare_covers_every_outcome() {
    use IdentityCheck as C;
    assert_eq!(C::compare(StampRead::Absent, Some(fp32())), C::Unknown);
    assert_eq!(
        C::compare(StampRead::Unreadable("x".into()), Some(fp32())),
        C::Unreadable("x".into())
    );
    assert_eq!(
        C::compare(StampRead::Stamped(fp32()), None),
        C::ActiveUnknown(fp32())
    );
    let other_dim = EmbedderIdentity::new(MODEL, 768, false);
    assert_eq!(
        C::compare(StampRead::Stamped(other_dim.clone()), Some(fp32())),
        C::Mismatch {
            stored: other_dim,
            active: fp32()
        }
    );
}

/// Why: only a run that replaced every vector may stamp.
/// Test: this test.
#[test]
fn should_stamp_only_full_rebuilds() {
    assert!(should_stamp(true, 10));
    assert!(should_stamp(false, 0));
    assert!(!should_stamp(false, 10));
}
