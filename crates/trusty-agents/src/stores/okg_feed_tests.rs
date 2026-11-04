//! The agent's OKG feed over `trusty_okf::feed` (#10746 S12).
//!
//! Why: SPEC-OKF-01 §4.9.1 (two machines, one synced tree, separate index
//! state) and §5.4 (no failure path records anything as indexed). These
//! drive `feed_source` against an in-process fake daemon per machine.
//! What: [`Fake`] answers the six methods the feed sends and remembers what
//! it holds; a fixture tree carries two ingested entities and a legacy
//! in-tree journal that claims both indexed.
//! Test: `cargo test -p trusty-agents stores::okg_feed`.

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use trusty_okf::build::policy::DocStorePolicy;
use trusty_okf::feed::SearchError;
use trusty_okf::profile::Profile;
use trusty_okf::state::{IndexJournal, IndexRecord};

use super::*;
use crate::kb::okg::registry::Locator;

/// One machine's search daemon.
pub(super) struct Fake {
    root: PathBuf,
    down: bool,
    refuse: Option<String>,
    /// #10746 S12 fix round: `(entered, release)` — the status call signals
    /// `entered`, then waits for `release`, so a test can hold a pass open.
    gate: Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>,
    held: Mutex<BTreeMap<String, String>>,
    calls: Mutex<Vec<String>>,
}

impl Fake {
    pub(super) fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            down: false,
            refuse: None,
            gate: None,
            held: Mutex::default(),
            calls: Mutex::default(),
        }
    }

    fn puts(&self) -> usize {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|m| *m == "search.index.file.put")
            .count()
    }

    fn under(&self, prefix: &str) -> Vec<Value> {
        self.held
            .lock()
            .unwrap()
            .iter()
            .filter(|(p, _)| p.starts_with(prefix))
            .map(|(p, c)| json!({"path": p, "content": c}))
            .collect()
    }
}

#[async_trait::async_trait]
impl trusty_okf::feed::IndexFeed for Fake {
    async fn call(&self, method: &str, p: Value) -> Result<Value, SearchError> {
        self.calls.lock().unwrap().push(method.to_string());
        if self.down {
            return Err(SearchError {
                method: method.to_string(),
                code: None,
                message: "connection refused".into(),
            });
        }
        if method == "search.index.status"
            && let Some((entered, release)) = &self.gate
        {
            entered.notify_one();
            release.notified().await;
        }
        let path = p["body"]["path"].as_str().unwrap_or_default().to_string();
        Ok(match method {
            "search.index.status" => json!({"root_path": self.root}),
            "search.chunks.list" => {
                let c = self.under(p["path_prefix"].as_str().unwrap_or_default());
                json!({"total": c.len(), "chunks": c})
            }
            "search.query" => {
                json!({"results": self.under(p["body"]["path_prefix"].as_str().unwrap_or_default())})
            }
            "search.index.file.put" if self.refuse.as_deref() == Some(path.as_str()) => {
                json!({"indexed": false, "reason": "no_chunks"})
            }
            "search.index.file.put" => {
                let content = p["body"]["content"].as_str().unwrap_or_default();
                self.held.lock().unwrap().insert(path, content.to_string());
                json!({"indexed": true})
            }
            "search.index.file.remove" => {
                self.held.lock().unwrap().remove(&path);
                json!({})
            }
            _ => json!({}),
        })
    }
}

/// A tree with two ingested entities and a legacy journal claiming both.
pub(super) fn fixture() -> (tempfile::TempDir, KbStore, SourceSpec) {
    let tmp = tempfile::tempdir().unwrap();
    let store = KbStore::new(tmp.path().join("kb"), Profile::default_profile());
    let corpus = tmp.path().join("corpus");
    std::fs::create_dir_all(&corpus).unwrap();
    std::fs::write(corpus.join("one.md"), "first note about atlas").unwrap();
    std::fs::write(corpus.join("two.md"), "second note about maya").unwrap();
    store
        .okg_register_source(SourceSpec::new(
            "corpus",
            Some("notes"),
            Locator::DocStore {
                path: corpus.to_string_lossy().to_string(),
                extensions: vec![],
                recursive: true,
            },
            "t0",
        ))
        .unwrap();
    let policy = DocStorePolicy::new(vec![tmp.path().canonicalize().unwrap()]);
    store
        .okg_ingest_docstore("corpus", &policy, "2026-07-24T00:00:00Z")
        .unwrap();
    let mut lines = String::new();
    for name in ["one", "two"] {
        let bytes = trusty_okf::state::ByteHash::of(
            &std::fs::read(store.root.join(format!("notes/{name}.md"))).unwrap(),
        );
        lines.push_str(&format!(
            "{{\"entity\":\"notes/{name}\",\"content_hash\":\"{}\",\"size\":1,\"state\":\"indexed\",\"at\":\"t0\"}}\n",
            bytes.as_str()
        ));
    }
    std::fs::write(store.root.join("_sources/corpus.index.jsonl"), lines).unwrap();
    let spec = store.okg_source("corpus").unwrap();
    (tmp, store, spec)
}

fn records(store: &KbStore, st: &IndexStateRef) -> Vec<IndexRecord> {
    IndexJournal::open(&st.dir, &store.root, &st.index_id)
        .unwrap()
        .read()
        .unwrap()
}

/// Whether any record is a confirmation (put, probe or remove).
fn confirmed(records: &[IndexRecord]) -> bool {
    records
        .iter()
        .any(|r| !matches!(r, IndexRecord::PutFailed { .. }))
}

/// Every file under `dir`, relative, sorted.
fn listing(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .map(|e| e.path().strip_prefix(dir).unwrap().to_path_buf())
        .collect();
    out.sort();
    out
}

/// Why: the r4 BLOCKER 5 defect, end to end. Two machines share one synced
/// tree whose legacy journal says both entities are indexed.
/// What: machine A feeds its daemon and reads (synced 0, pending 0): both
/// are pushed and probed, but with no index identity nothing reads synced
/// (§4.9.2 rule 5; #10746 S12 fix round).
/// Machine B, with its own state directory and an empty daemon, reads
/// (0, 2) before its pass — neither A's state nor the legacy journal leaks
/// — and its pass pushes both files to B's daemon. A's journal is unchanged
/// by B's pass, and neither pass creates a file in the tree.
/// Test: self-contained.
#[tokio::test]
async fn two_machines_over_one_tree_keep_separate_index_state() {
    let (tmp, store, spec) = fixture();
    let tree_before = listing(&store.root);
    let a = IndexStateRef::new(tmp.path().join("machine-a"), "idx");
    let b = IndexStateRef::new(tmp.path().join("machine-b"), "idx");
    let fake_a = Fake::new(&store.root);
    let fake_b = Fake::new(&store.root);

    let ra = feed_source(&store, &spec, &a, &fake_a).await;
    assert_eq!(ra.index.as_deref(), Some("idx"), "{ra:?}");
    assert_eq!((ra.indexed, ra.synced, ra.pending), (2, 0, 0), "{ra:?}");
    assert_eq!(
        ra.status,
        Some(FeedStatus::Unconfirmed),
        "no index identity yet (D1), so never Searchable"
    );
    assert!(ra.acknowledged("idx"));
    assert_eq!(
        fake_a.puts(),
        2,
        "the legacy claim is re-proved, not trusted"
    );

    let view_b = store.index_view(Some(&b)).unwrap();
    let cov_b = store.okg_index_coverage_of(&spec, view_b.as_ref()).unwrap();
    assert_eq!(
        (cov_b.synced, cov_b.pending),
        (0, 2),
        "B never reads A's state"
    );

    let a_bytes = std::fs::read(
        IndexJournal::open(&a.dir, &store.root, "idx")
            .unwrap()
            .path(),
    )
    .unwrap();
    let rb = feed_source(&store, &spec, &b, &fake_b).await;
    assert_eq!((rb.indexed, rb.synced, rb.pending), (2, 0, 0), "{rb:?}");
    assert_eq!(fake_b.puts(), 2, "B's own daemon received both");
    assert_eq!(
        std::fs::read(
            IndexJournal::open(&a.dir, &store.root, "idx")
                .unwrap()
                .path()
        )
        .unwrap(),
        a_bytes,
        "B's pass never touches A's state"
    );
    assert_eq!(
        listing(&store.root),
        tree_before,
        "no pass writes into the tree"
    );
}

/// Why: §5.4 F1 — a dead daemon feeds nothing and records nothing.
/// What: the report is not fed, names the index and the daemon, counts both
/// entities pending, and the state journal holds no confirmation.
/// Test: self-contained.
#[tokio::test]
async fn daemon_down_is_not_fed_and_records_nothing() {
    let (tmp, store, spec) = fixture();
    let st = IndexStateRef::new(tmp.path().join("state"), "idx");
    let mut fake = Fake::new(&store.root);
    fake.down = true;
    let r = feed_source(&store, &spec, &st, &fake).await;
    assert!(r.index.is_none(), "{r:?}");
    let reason = r.reason.clone().unwrap_or_default();
    assert!(
        reason.contains("could not be interrogated") && reason.contains("idx"),
        "{reason}"
    );
    assert_eq!((r.pending, r.synced), (2, 0));
    assert!(!r.acknowledged("idx"));
    assert!(!confirmed(&records(&store, &st)));
}

/// Why: §5.4 F5 — an index rooted away from the tree would hide every push.
/// What: the pass is refused before any put, with the remediation text, and
/// nothing is confirmed. Test: self-contained.
#[tokio::test]
async fn root_mismatch_is_refused_before_any_push() {
    let (tmp, store, spec) = fixture();
    let st = IndexStateRef::new(tmp.path().join("state"), "idx");
    let elsewhere = tempfile::tempdir().unwrap();
    let fake = Fake::new(elsewhere.path());
    let r = feed_source(&store, &spec, &st, &fake).await;
    assert!(
        r.reason
            .unwrap_or_default()
            .contains("does not contain this tree")
    );
    assert_eq!(fake.puts(), 0);
    assert!(!confirmed(&records(&store, &st)));
}

/// Why: §5.4 F2 and F4 — a put the daemon refuses (`indexed: false`) is a
/// failed task, never an acknowledgement.
/// What: one of two puts is refused; the pass is Partial, names the path,
/// leaves it pending, journals no acknowledgement for it, and is not
/// acknowledged. Test: self-contained.
#[tokio::test]
async fn refused_put_is_partial_and_records_nothing() {
    let (tmp, store, spec) = fixture();
    let st = IndexStateRef::new(tmp.path().join("state"), "idx");
    let mut fake = Fake::new(&store.root);
    fake.refuse = Some("notes/two.md".into());
    let r = feed_source(&store, &spec, &st, &fake).await;
    assert_eq!(r.status, Some(FeedStatus::Partial), "{r:?}");
    assert_eq!((r.indexed, r.pending, r.synced), (1, 1, 0));
    assert!(
        r.errors.iter().any(|e| e.starts_with("notes/two.md")),
        "{r:?}"
    );
    assert!(!r.acknowledged("idx"));
    let acked_two = records(&store, &st)
        .iter()
        .any(|rec| matches!(rec, IndexRecord::PutAck { path, .. } if path == "notes/two.md"));
    assert!(!acked_two);
}

/// Why: §5.4 F3 — a confirmed put whose journal line cannot be written is
/// not indexed, and the pass stops.
/// What: the state journal is read-only after the migration; the pass
/// returns not fed with the reconcile reason and the journal is unchanged.
/// Test: self-contained.
#[tokio::test]
async fn journal_write_failure_is_not_fed() {
    let (tmp, store, spec) = fixture();
    let st = IndexStateRef::new(tmp.path().join("state"), "idx");
    crate::kb::okg::legacy_journal::migrate_legacy_journals(&store.root, &st).unwrap();
    let path = IndexJournal::open(&st.dir, &store.root, "idx")
        .unwrap()
        .path()
        .to_path_buf();
    let before = std::fs::read(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
    let fake = Fake::new(&store.root);
    let r = feed_source(&store, &spec, &st, &fake).await;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(r.index.is_none(), "{r:?}");
    assert!(
        r.reason
            .unwrap_or_default()
            .contains("index reconcile failed")
    );
    assert_eq!(std::fs::read(&path).unwrap(), before, "nothing journaled");
    assert_eq!((r.pending, r.synced), (2, 0));
}

/// Why: a migration that cannot finish must not let a pass run over half a
/// journal. What: an unreadable legacy journal fails the migration; the
/// report is not fed, the daemon is never called, and nothing is confirmed.
/// Test: self-contained.
#[tokio::test]
async fn migration_failure_is_not_fed() {
    let (tmp, store, spec) = fixture();
    let st = IndexStateRef::new(tmp.path().join("state"), "idx");
    std::fs::create_dir_all(store.root.join("_sources/broken.index.jsonl")).unwrap();
    let fake = Fake::new(&store.root);
    let r = feed_source(&store, &spec, &st, &fake).await;
    assert!(r.reason.unwrap_or_default().contains("migration failed"));
    assert!(fake.calls.lock().unwrap().is_empty(), "no daemon call");
    assert!(!confirmed(&records(&store, &st)));
}

/// Why: #10746 S12 fix round, CRITICAL — the pass covers the whole tree,
/// and §5.4 F2 says one failing concept does not stop the others. A
/// non-UTF-8 concept of no source (or another source's path the index
/// refuses) must not block this source's publication.
/// What: a non-UTF-8 `misc/bad.md` sits beside the source's two entities;
/// the pass reports it as an error, and the source's publication is still
/// acknowledged. Test: self-contained.
#[tokio::test]
async fn unrelated_unreadable_concept_does_not_block_publication() {
    let (tmp, store, spec) = fixture();
    std::fs::create_dir_all(store.root.join("misc")).unwrap();
    std::fs::write(store.root.join("misc/bad.md"), [0xff, 0xfe, 0x00]).unwrap();
    let st = IndexStateRef::new(tmp.path().join("state"), "idx");
    let fake = Fake::new(&store.root);
    let r = feed_source(&store, &spec, &st, &fake).await;
    assert!(
        r.errors.iter().any(|e| e.contains("misc/bad.md")),
        "the unrelated failure stays in the report: {r:?}"
    );
    assert!(
        r.acknowledged("idx"),
        "and does not block this source: {r:?}"
    );
}

/// Why: the other half of the per-source gate — a refused put of the
/// source's OWN entity must still block its publication.
/// What: the daemon refuses `notes/two.md`; the source is not acknowledged.
/// Test: self-contained.
#[tokio::test]
async fn refused_put_of_own_entity_blocks_publication() {
    let (tmp, store, spec) = fixture();
    let st = IndexStateRef::new(tmp.path().join("state"), "idx");
    let mut fake = Fake::new(&store.root);
    fake.refuse = Some("notes/two.md".into());
    let r = feed_source(&store, &spec, &st, &fake).await;
    assert!(!r.acknowledged("idx"), "{r:?}");
}

/// Why: #10746 S12 fix round, HIGH — two concurrent feed calls must not
/// each run a full-tree pass over one state journal.
/// What: call A holds its pass open inside the status call; call B, over
/// the same tree and state, reports `feed already running` and never calls
/// its daemon; A then finishes normally. Test: self-contained.
#[tokio::test]
async fn concurrent_feed_calls_run_one_pass() {
    let (tmp, store, spec) = fixture();
    let st = IndexStateRef::new(tmp.path().join("state"), "idx");
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let mut held = Fake::new(&store.root);
    held.gate = Some((entered.clone(), release.clone()));
    let other = Fake::new(&store.root);
    let (a, b) = tokio::join!(feed_source(&store, &spec, &st, &held), async {
        entered.notified().await;
        let b = feed_source(&store, &spec, &st, &other).await;
        release.notify_one();
        b
    });
    let reason = b.reason.clone().unwrap_or_default();
    assert!(reason.contains("feed already running"), "{b:?}");
    assert!(other.calls.lock().unwrap().is_empty(), "B started no pass");
    assert_eq!((a.reason, a.indexed), (None, 2), "A ran its pass");
}
