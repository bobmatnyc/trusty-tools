//! SPEC-OKF-01 §6.2 criteria 43-51, 133 and 134, and the feed halves of
//! 60, 63 and 69, for slice S7 of #10746 (feature `search-feed`).
//!
//! Why: every feed failure path must leave the journal unchanged and report
//! failure, and only a passed probe under a known identity reads Searchable.
//! What: [`Fake`] is a scripted fake daemon. It answers the trusty-search
//! methods the feed sends (status, chunks.list, query, file.put,
//! file.remove, files.remove and, with D1, file.status) from an in-memory
//! corpus, and each test scripts one fault. Tests run it in-process through
//! `IndexFeed`, and once over a real Unix socket through `RpcIndexFeed`.
//! Every Searchable expectation runs with D1; the same flows without D1 end
//! `Unconfirmed`.
//! Test: `cargo test -p trusty-okf --features search-feed --test feed_criteria`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, FixedOffset};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use trusty_okf::OkfError;
use trusty_okf::bundle::{Bundle, PutOutcome, VerificationState};
use trusty_okf::feed::{
    FeedReport, FeedStatus, IndexFeed, RpcIndexFeed, SearchError, exit_code, feed,
};
use trusty_okf::format::{Actor, Concept, ConceptId, Verification, WriteMode, content_hash};
use trusty_okf::retrieve::{Query, retrieve};
use trusty_okf::state::{
    IndexIdentity, IndexJournal, IndexRecord, IndexState, IndexStateDir, LiveIdentity, TaskOp,
};

const INDEX: &str = "kb-feed";

fn now() -> DateTime<FixedOffset> {
    DateTime::parse_from_rfc3339("2026-11-04T12:00:00Z").unwrap()
}

fn id(s: &str) -> ConceptId {
    ConceptId::parse(s).unwrap()
}

fn hex(s: &str) -> String {
    Sha256::digest(s.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A side effect a put runs, given the wire path.
type PutHook = Box<dyn Fn(&str) + Send + Sync>;

/// What the fake does differently from a well-behaved daemon.
#[derive(Default)]
struct Script {
    /// Every call fails as a transport error (daemon gone mid-pass).
    down: bool,
    /// Wire paths whose put answers a daemon error.
    fail_put: BTreeSet<String>,
    /// Wire paths whose put answers `indexed: false`.
    indexed_false: BTreeSet<String>,
    /// Wire paths whose put answers a tombstone `removed: true`.
    removed_true: BTreeSet<String>,
    /// A put echoes a hash that is not the content's (D1 only).
    wrong_hash: bool,
    /// A put acknowledges and echoes the new hash, and a remove confirms,
    /// but both keep serving the old chunks and the old stored hash
    /// (criterion 133).
    stale: bool,
    /// A put appends chunks instead of replacing them (pre-#8959 daemon).
    append_on_put: bool,
    /// The query lane returns nothing (criterion 63).
    hide_query: bool,
    /// The query lane answers an error: the probe cannot run (F13).
    query_error: bool,
    /// Every `file.remove` answers an error.
    fail_remove: bool,
    /// The `identity` a `file.status` reply carries instead of the live one
    /// (`Value::Null` is D1's unreadable-identity reply).
    file_status_identity: Option<Value>,
    /// The chunker drops a path's last paragraph.
    lossy: bool,
    /// The status reply's `root_path`, when not the fake's root.
    root_override: Option<Value>,
    /// Runs inside every put, with the wire path.
    on_put: Option<PutHook>,
}

/// The scripted fake daemon.
struct Fake {
    root: PathBuf,
    identity: Mutex<Option<IndexIdentity>>,
    corpus: Mutex<BTreeMap<String, Vec<String>>>,
    hashes: Mutex<BTreeMap<String, String>>,
    calls: Mutex<Vec<(String, Value)>>,
    script: Mutex<Script>,
}

impl Fake {
    fn new(root: &Path, d1: bool) -> Arc<Fake> {
        Arc::new(Fake {
            root: root.to_path_buf(),
            identity: Mutex::new(d1.then(|| ident("inst-1", 1))),
            corpus: Mutex::default(),
            hashes: Mutex::default(),
            calls: Mutex::default(),
            script: Mutex::default(),
        })
    }

    fn script(&self, f: impl FnOnce(&mut Script)) {
        f(&mut self.script.lock().unwrap());
    }

    fn calls(&self, method: &str) -> Vec<Value> {
        let calls = self.calls.lock().unwrap();
        calls
            .iter()
            .filter(|(m, _)| m == method)
            .map(|(_, p)| p.clone())
            .collect()
    }

    fn clear_calls(&self) {
        self.calls.lock().unwrap().clear();
    }

    /// Delete and re-create the index: a new instance, an empty corpus.
    fn recreate(&self, instance: &str) {
        *self.identity.lock().unwrap() = Some(ident(instance, 1));
        self.corpus.lock().unwrap().clear();
        self.hashes.lock().unwrap().clear();
    }

    /// #10876: the daemon is rolled back to a build without D1; replies
    /// carry no identity. The corpus is kept.
    fn drop_d1(&self) {
        *self.identity.lock().unwrap() = None;
    }

    /// A generation bump with the same instance: the corpus lost content.
    fn bump_generation(&self) {
        if let Some(i) = self.identity.lock().unwrap().as_mut() {
            i.generation += 1;
        }
        self.corpus.lock().unwrap().clear();
        self.hashes.lock().unwrap().clear();
    }

    fn seed(&self, wire: &str, text: &str) {
        self.corpus
            .lock()
            .unwrap()
            .entry(wire.to_string())
            .or_default()
            .push(text.to_string());
    }

    fn chunks_of(&self, wire: &str) -> Vec<String> {
        self.corpus
            .lock()
            .unwrap()
            .get(wire)
            .cloned()
            .unwrap_or_default()
    }

    fn chunk_json(&self, wire: &str, text: &str) -> Value {
        json!({"path": wire, "file": self.root.join(wire), "content": text,
               "start_line": 1, "end_line": 1, "score": 1.0})
    }

    fn under(&self, prefix: &str) -> Vec<Value> {
        let corpus = self.corpus.lock().unwrap();
        corpus
            .iter()
            .filter(|(p, _)| *p == prefix || p.starts_with(&format!("{prefix}/")))
            .flat_map(|(p, cs)| cs.iter().map(move |c| (p.clone(), c.clone())))
            .map(|(p, c)| self.chunk_json(&p, &c))
            .collect()
    }

    fn handle(&self, method: &str, p: &Value) -> Result<Value, SearchError> {
        let err = |code: Option<i64>, message: &str| SearchError {
            method: method.to_string(),
            code,
            message: message.to_string(),
        };
        let s = self.script.lock().unwrap();
        if s.down {
            return Err(err(None, "connection refused"));
        }
        let identity = self.identity.lock().unwrap().clone();
        let with_identity = |mut v: Value| {
            if let Some(i) = &identity {
                v["identity"] = json!(i);
            }
            v
        };
        let path = p["body"]["path"].as_str().unwrap_or_default().to_string();
        match method {
            "search.index.status" => Ok(with_identity(json!({
                "index_id": INDEX,
                "root_path": s.root_override.clone().unwrap_or_else(|| json!(self.root)),
            }))),
            "search.chunks.list" => {
                let all = self.under(p["path_prefix"].as_str().unwrap_or_default());
                let off = p["offset"].as_u64().unwrap_or(0) as usize;
                let lim = p["limit"].as_u64().unwrap_or(100) as usize;
                let page: Vec<Value> = all.iter().skip(off).take(lim).cloned().collect();
                Ok(json!({"total": all.len(), "chunks": page}))
            }
            "search.query" if s.query_error => Err(err(Some(-32000), "embedder stalled")),
            "search.index.file.remove" if s.fail_remove => Err(err(Some(-32000), "remove failed")),
            "search.query" => {
                if s.hide_query {
                    return Ok(json!({"results": []}));
                }
                let prefix = p["body"]["path_prefix"].as_str().unwrap_or_default();
                let words: Vec<String> = p["body"]["text"]
                    .as_str()
                    .unwrap_or_default()
                    .split_whitespace()
                    .map(str::to_lowercase)
                    .collect();
                let hits: Vec<Value> = self
                    .under(prefix)
                    .into_iter()
                    .filter(|c| {
                        let t = c["content"].as_str().unwrap_or_default().to_lowercase();
                        words.iter().any(|w| t.contains(w.as_str()))
                    })
                    .collect();
                Ok(json!({ "results": hits }))
            }
            "search.index.file.put" => {
                if let Some(hook) = &s.on_put {
                    hook(&path);
                }
                let content = p["body"]["content"].as_str().unwrap_or_default();
                if s.fail_put.contains(&path) {
                    return Err(err(Some(-32000), "put refused"));
                }
                if s.indexed_false.contains(&path) {
                    return Ok(json!({"path": path, "indexed": false, "reason": "no_chunks"}));
                }
                let echoed = if s.wrong_hash {
                    hex("other")
                } else {
                    hex(content)
                };
                if s.removed_true.contains(&path) {
                    self.corpus.lock().unwrap().remove(&path);
                    return Ok(with_identity(
                        json!({"path": path, "removed": true, "content_sha256": echoed}),
                    ));
                }
                if !s.stale {
                    let mut paras: Vec<String> = content
                        .split("\n\n")
                        .filter(|x| !x.trim().is_empty())
                        .map(str::to_string)
                        .collect();
                    if s.lossy {
                        paras.pop();
                    }
                    let mut corpus = self.corpus.lock().unwrap();
                    let slot = corpus.entry(path.clone()).or_default();
                    if !s.append_on_put {
                        slot.clear();
                    }
                    slot.extend(paras);
                    self.hashes
                        .lock()
                        .unwrap()
                        .insert(path.clone(), hex(content));
                }
                let mut reply = json!({"path": path, "indexed": true});
                if identity.is_some() {
                    reply["content_sha256"] = json!(echoed);
                }
                Ok(with_identity(reply))
            }
            "search.index.file.remove" if s.stale => Ok(json!({"path": path, "removed": true})),
            "search.index.file.remove" => {
                self.corpus.lock().unwrap().remove(&path);
                self.hashes.lock().unwrap().remove(&path);
                Ok(json!({"path": path, "removed": true}))
            }
            "search.index.files.remove" => {
                let prefix = p["body"]["path_prefix"].as_str().unwrap_or_default();
                let mut corpus = self.corpus.lock().unwrap();
                corpus.retain(|k, _| !(k == prefix || k.starts_with(&format!("{prefix}/"))));
                Ok(json!({"removed": true}))
            }
            "search.index.file.status" if identity.is_some() => {
                let present = !self.chunks_of(&path).is_empty();
                let hash = self.hashes.lock().unwrap().get(&path).cloned();
                let mut reply = with_identity(json!({"path": path, "present": present,
                    "content_sha256": if present { json!(hash) } else { Value::Null },
                    "chunk_count": self.chunks_of(&path).len()}));
                if let Some(i) = &s.file_status_identity {
                    reply["identity"] = i.clone();
                }
                Ok(reply)
            }
            _ => Err(err(Some(-32601), "method not found")),
        }
    }
}

#[async_trait::async_trait]
impl IndexFeed for Fake {
    async fn call(&self, method: &str, params: Value) -> Result<Value, SearchError> {
        self.calls
            .lock()
            .unwrap()
            .push((method.to_string(), params.clone()));
        self.handle(method, &params)
    }
}

fn ident(instance: &str, generation: u64) -> IndexIdentity {
    IndexIdentity {
        instance_id: instance.into(),
        generation,
    }
}

/// An index root holding the bundle at `bundle/`, and a state directory.
struct Fx {
    _tmp: tempfile::TempDir,
    index_root: PathBuf,
    root: PathBuf,
    state: IndexStateDir,
}

fn fx() -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let index_root = base.join("idx");
    let root = index_root.join("bundle");
    std::fs::create_dir_all(&root).unwrap();
    Fx {
        state: IndexStateDir::new(base.join("machine-a")),
        index_root,
        root,
        _tmp: tmp,
    }
}

impl Fx {
    fn bundle(&self) -> Bundle {
        Bundle::open(&self.root).unwrap().0
    }

    fn journal(&self) -> IndexJournal {
        IndexJournal::open(&self.state, &self.root, INDEX).unwrap()
    }

    fn records(&self) -> Vec<IndexRecord> {
        self.journal().read().unwrap()
    }

    fn journal_bytes(&self) -> Option<Vec<u8>> {
        std::fs::read(self.journal().path()).ok()
    }

    fn live(&self, fake: &Fake) -> LiveIdentity {
        match fake.identity.lock().unwrap().clone() {
            Some(i) => LiveIdentity::Known(i),
            None => LiveIdentity::Unsupported,
        }
    }

    fn index(&self, name: &str, fake: &Fake) -> IndexState {
        self.bundle()
            .revision_state(&id(name), &self.state, INDEX, &self.live(fake))
            .unwrap()
            .index
    }

    fn synced(&self, fake: &Fake) -> bool {
        self.bundle()
            .state_summary(&self.state, INDEX, &self.live(fake))
            .unwrap()
            .synced()
    }

    async fn feed(&self, fake: &Fake) -> Result<FeedReport, OkfError> {
        feed(&self.bundle(), &self.state, INDEX, fake).await
    }
}

/// Write concept `name` through `put` with two paragraphs and more.
fn create(root: &Path, name: &str, paras: &[&str]) {
    let mut b = Bundle::open(root).unwrap().0;
    let mut m = serde_yaml::Mapping::new();
    m.insert("type".into(), "Note".into());
    m.insert("title".into(), name.into());
    let body = format!("{}\n", paras.join("\n\n"));
    let c = Concept::new(id(name), m, body);
    let (out, _) = b
        .put(c, Actor::Process("test".into()), WriteMode::Lenient, now())
        .unwrap();
    assert_eq!(out, PutOutcome::Created);
}

/// Replace `name`'s body through `put`.
fn edit(root: &Path, name: &str, paras: &[&str]) {
    let mut b = Bundle::open(root).unwrap().0;
    let mut c = b.get(&id(name)).unwrap();
    c.body = format!("{}\n", paras.join("\n\n"));
    b.put(c, Actor::Process("test".into()), WriteMode::Lenient, now())
        .unwrap();
}

fn put_acks(records: &[IndexRecord]) -> Vec<&str> {
    records
        .iter()
        .filter_map(|r| match r {
            IndexRecord::PutAck { path, .. } => Some(path.as_str()),
            _ => None,
        })
        .collect()
}

fn names_path(r: &IndexRecord, p: &str) -> bool {
    match r {
        IndexRecord::PutAck { path, .. }
        | IndexRecord::PutFailed { path, .. }
        | IndexRecord::Probe { path, .. }
        | IndexRecord::RemoveAck { path, .. }
        | IndexRecord::WithdrawProbe { path, .. } => path == p,
        IndexRecord::Reindex { .. } | IndexRecord::Observed { .. } => false,
    }
}

/// #10876: a pass under D1 journals the identity it read; whether the
/// journal holds nothing else.
fn only_observed(records: &[IndexRecord]) -> bool {
    records
        .iter()
        .all(|r| matches!(r, IndexRecord::Observed { .. }))
}

fn observed(records: &[IndexRecord]) -> Vec<Option<IndexIdentity>> {
    records
        .iter()
        .filter_map(|r| match r {
            IndexRecord::Observed { identity } => Some(identity.clone()),
            _ => None,
        })
        .collect()
}

const ALPHA: [&str; 3] = [
    "Alpha opens the first paragraph with words",
    "Alpha middle paragraph carries more words",
    "Alpha closing paragraph ends here",
];

/// Why: criterion 43 (F1). What: a real socket client dialling an absent
/// socket. Test: self-contained.
#[tokio::test]
async fn c43_daemon_down_is_not_fed_and_the_journal_is_unchanged() {
    let f = fx();
    create(&f.root, "a", &ALPHA);
    create(&f.root, "b", &["Beta words"]);
    let backlog = f
        .bundle()
        .index_backlog(&f.state, INDEX, &LiveIdentity::Unsupported)
        .unwrap();
    let before = f.journal_bytes();
    let dead = RpcIndexFeed::new(f.root.parent().unwrap().join("absent.sock"));
    let r = feed(&f.bundle(), &f.state, INDEX, &dead).await.unwrap();
    assert_eq!(r.status, FeedStatus::NotFed);
    assert_eq!(r.pending, backlog.owed());
    assert_eq!(r.pending, 2);
    assert!(r.errors[0].message.contains("absent.sock"), "{r:?}");
    assert_eq!(f.journal_bytes(), before);
    assert_ne!(exit_code(&Ok(r)), 0);
}

/// Why: criterion 60, feed half (§1.1 case 1). What: a put lands, the feed
/// pass fails with the daemon down. Test: self-contained.
#[tokio::test]
async fn c60_put_lands_and_a_failed_feed_reads_saved_and_pending() {
    let f = fx();
    create(&f.root, "a", &ALPHA);
    let fake = Fake::new(&f.index_root, true);
    fake.script(|s| s.down = true);
    let r = f.feed(&fake).await.unwrap();
    assert_eq!(r.status, FeedStatus::NotFed);
    assert_eq!((r.acknowledged, r.searchable), (0, 0));
    let st = f
        .bundle()
        .revision_state(&id("a"), &f.state, INDEX, &f.live(&fake))
        .unwrap();
    assert_eq!(st.save, trusty_okf::state::SaveState::Saved);
    assert!(matches!(st.index, IndexState::Pending { .. }), "{st:?}");
    assert!(!f.synced(&fake));
    assert_eq!(exit_code(&Ok(r)), 4);
}

/// Why: criterion 44 (F2). What: three concepts, one put refused.
/// Test: self-contained.
#[tokio::test]
async fn c44_one_failed_put_of_three_is_partial() {
    let f = fx();
    for n in ["a", "b", "c"] {
        create(&f.root, n, &[&format!("{n} body words")]);
    }
    let fake = Fake::new(&f.index_root, true);
    fake.script(|s| {
        s.fail_put.insert("bundle/b.md".into());
    });
    let r = f.feed(&fake).await.unwrap();
    assert_eq!(r.status, FeedStatus::Partial);
    assert_eq!(r.errors.len(), 1, "{r:?}");
    assert_eq!(r.errors[0].path, "b.md");
    assert_eq!((r.acknowledged, r.pending), (2, 1));
    let rec = f.records();
    assert_eq!(put_acks(&rec), vec!["a.md", "c.md"]);
    assert!(!rec.iter().any(|r| names_path(r, "b.md")), "{rec:?}");
    assert!(matches!(f.index("b", &fake), IndexState::Pending { .. }));
}

/// Why: criterion 45 (F3). What: the fake makes the journal read-only while
/// it confirms the put. Test: self-contained.
#[tokio::test]
async fn c45_journal_append_failure_counts_nothing_and_the_next_pass_repushes() {
    use std::os::unix::fs::PermissionsExt;
    let f = fx();
    create(&f.root, "a", &ALPHA);
    let fake = Fake::new(&f.index_root, true);
    let jpath = f.journal().path().to_path_buf();
    let j2 = jpath.clone();
    fake.script(|s| {
        s.on_put = Some(Box::new(move |_| {
            std::fs::create_dir_all(j2.parent().unwrap()).unwrap();
            std::fs::write(&j2, b"").unwrap();
            std::fs::set_permissions(&j2, std::fs::Permissions::from_mode(0o444)).unwrap();
        }));
    });
    let got = f.feed(&fake).await;
    assert!(matches!(got, Err(OkfError::JournalWrite { .. })), "{got:?}");
    assert_eq!(exit_code(&got), 1);
    assert!(f.records().is_empty());
    assert!(matches!(f.index("a", &fake), IndexState::Pending { .. }));

    std::fs::set_permissions(&jpath, std::fs::Permissions::from_mode(0o644)).unwrap();
    fake.script(|s| s.on_put = None);
    fake.clear_calls();
    let r = f.feed(&fake).await.unwrap();
    assert_eq!(fake.calls("search.index.file.put").len(), 1);
    assert_eq!(r.status, FeedStatus::Searchable);
}

/// Why: criterion 46 (F4). What: `indexed: false` for one path, a tombstone
/// `removed: true` for another. Test: self-contained.
#[tokio::test]
async fn c46_indexed_false_appends_nothing_and_removed_true_succeeds() {
    let f = fx();
    create(&f.root, "a", &ALPHA);
    create(&f.root, "t", &["Tombstone words"]);
    let fake = Fake::new(&f.index_root, true);
    fake.script(|s| {
        s.indexed_false.insert("bundle/a.md".into());
        s.removed_true.insert("bundle/t.md".into());
    });
    let r = f.feed(&fake).await.unwrap();
    assert_eq!(r.status, FeedStatus::Partial);
    assert_eq!((r.acknowledged, r.removed, r.pending), (0, 1, 1));
    assert!(r.errors[0].message.contains("indexed: false"), "{r:?}");
    let rec = f.records();
    assert!(!rec.iter().any(|r| names_path(r, "a.md")), "{rec:?}");
    assert!(
        rec.iter()
            .any(|r| matches!(r, IndexRecord::RemoveAck { path, .. } if path == "t.md"))
    );
}

/// Why: the feed counts a put only when `content_sha256` matches the bytes
/// sent (§5.6). What: a D1 fake that echoes another hash. Test: itself.
#[tokio::test]
async fn put_with_a_wrong_content_sha256_is_not_counted() {
    let f = fx();
    create(&f.root, "a", &ALPHA);
    let fake = Fake::new(&f.index_root, true);
    fake.script(|s| s.wrong_hash = true);
    let r = f.feed(&fake).await.unwrap();
    assert_eq!(r.status, FeedStatus::NotFed);
    assert_eq!(r.acknowledged, 0);
    assert!(r.errors[0].message.contains("content_sha256"), "{r:?}");
    // #10876: the identity observation is the only line.
    assert!(only_observed(&f.records()));
}

/// Why: criterion 47 (F5). What: a root elsewhere, a relative root and no
/// root. Test: self-contained.
#[tokio::test]
async fn c47_root_mismatch_refuses_before_any_put() {
    let f = fx();
    create(&f.root, "a", &ALPHA);
    for root in [json!("/elsewhere"), json!("relative/root"), Value::Null] {
        let fake = Fake::new(&f.index_root, true);
        fake.script(|s| s.root_override = Some(root.clone()));
        let got = f.feed(&fake).await;
        assert!(matches!(got, Err(OkfError::RootMismatch { .. })), "{got:?}");
        assert_eq!(exit_code(&got), 2);
        assert!(fake.calls("search.index.file.put").is_empty());
        assert!(f.journal_bytes().is_none());
    }
}

/// Why: criterion 48 (F6). What: a daemon that appends chunks on put; the
/// feed's remove-then-put leaves only the shorter revision's chunks.
/// Test: self-contained.
#[tokio::test]
async fn c48_shorter_revision_leaves_only_new_chunks() {
    let f = fx();
    create(&f.root, "a", &ALPHA);
    let fake = Fake::new(&f.index_root, true);
    fake.script(|s| s.append_on_put = true);
    assert_eq!(f.feed(&fake).await.unwrap().status, FeedStatus::Searchable);
    edit(&f.root, "a", &["Alpha short revision words"]);
    let r = f.feed(&fake).await.unwrap();
    assert_eq!(r.status, FeedStatus::Searchable, "{r:?}");
    let chunks = fake.chunks_of("bundle/a.md");
    let text = std::fs::read_to_string(f.root.join("a.md")).unwrap();
    assert!(
        chunks.iter().all(|c| text.contains(c.as_str())),
        "{chunks:?}"
    );
    assert!(!chunks.iter().any(|c| c.contains("middle paragraph")));
}

/// Why: criterion 49. What: the file changes after the feed read it for the
/// put. The journal names the bytes sent; the next backlog lists the path.
/// Test: self-contained.
#[tokio::test]
async fn c49_journal_records_the_bytes_sent() {
    let f = fx();
    create(&f.root, "a", &ALPHA);
    let fake = Fake::new(&f.index_root, true);
    let file = f.root.join("a.md");
    fake.script(|s| {
        s.on_put = Some(Box::new(move |_| {
            let t = std::fs::read_to_string(&file).unwrap();
            std::fs::write(&file, format!("{t}\nEdited after the read.\n")).unwrap();
        }));
    });
    f.feed(&fake).await.unwrap();
    let sent = fake.calls("search.index.file.put")[0]["body"]["content"]
        .as_str()
        .unwrap()
        .to_string();
    let rec = f.records();
    let acked = rec.iter().find_map(|r| match r {
        IndexRecord::PutAck { bytes, .. } => Some(bytes.clone()),
        _ => None,
    });
    assert_eq!(acked, Some(format!("sha256:{}", hex(&sent))));
    let backlog = f
        .bundle()
        .index_backlog(&f.state, INDEX, &f.live(&fake))
        .unwrap();
    assert_eq!(backlog.task("a.md"), Some(TaskOp::Push { replace: true }));
}

/// Why: criterion 50. What: a hand-written concept and a moved concept.
/// Test: self-contained.
#[tokio::test]
async fn c50_hand_owned_concept_and_moved_path_enter_the_backlog() {
    let f = fx();
    create(&f.root, "a", &ALPHA);
    let fake = Fake::new(&f.index_root, true);
    assert_eq!(f.feed(&fake).await.unwrap().status, FeedStatus::Searchable);
    std::fs::write(f.root.join("hand.md"), "---\ntype: Note\n---\nHand words\n").unwrap();
    let mut b = f.bundle();
    b.move_concept(&id("a"), &id("moved/a"), now()).unwrap();
    let backlog = b.index_backlog(&f.state, INDEX, &f.live(&fake)).unwrap();
    assert_eq!(
        backlog.task("hand.md"),
        Some(TaskOp::Push { replace: false })
    );
    assert_eq!(
        backlog.task("moved/a.md"),
        Some(TaskOp::Push { replace: false })
    );
    assert_eq!(backlog.task("a.md"), Some(TaskOp::Remove));
    let r = f.feed(&fake).await.unwrap();
    assert_eq!(r.status, FeedStatus::Searchable, "{r:?}");
    assert!(fake.chunks_of("bundle/a.md").is_empty());
}

/// Why: criterion 51 (3g B, 3e A). What: the library's exit-code mapping;
/// the crate has no CLI binary yet (B7, S8). Test: self-contained.
#[test]
fn c51_exit_code_is_non_zero_unless_searchable() {
    let report = |status| FeedReport {
        status,
        acknowledged: 0,
        searchable: 0,
        removed: 0,
        pending: 0,
        errors: Vec::new(),
    };
    assert_eq!(report(FeedStatus::Searchable).exit_code(), 0);
    for s in [
        FeedStatus::Unconfirmed,
        FeedStatus::Partial,
        FeedStatus::NotFed,
    ] {
        assert_ne!(exit_code(&Ok(report(s))), 0, "{s:?}");
    }
}

/// Why: criterion 63, feed half (F12). What: puts confirmed, the query lane
/// returns nothing; with and without D1. Test: self-contained.
#[tokio::test]
async fn c63_acknowledgement_alone_is_not_searchable_through_the_feed() {
    for d1 in [true, false] {
        let f = fx();
        create(&f.root, "a", &ALPHA);
        let fake = Fake::new(&f.index_root, d1);
        fake.script(|s| s.hide_query = true);
        let r = f.feed(&fake).await.unwrap();
        assert_eq!(r.status, FeedStatus::Unconfirmed);
        assert_eq!((r.acknowledged, r.searchable), (1, 0));
        let want = if d1 {
            IndexState::Acknowledged
        } else {
            IndexState::Pending {
                last_error: Some("index_identity_unsupported".into()),
            }
        };
        assert_eq!(f.index("a", &fake), want);
        assert!(!f.synced(&fake));
        assert_eq!(exit_code(&Ok(r)), 4);
    }
}

/// Why: §4.9.2 rule 5 through the real feed. What: a daemon without D1
/// confirms every put and every probe passes. Test: itself.
#[tokio::test]
async fn no_state_reads_searchable_without_d1_through_the_feed() {
    let f = fx();
    create(&f.root, "a", &ALPHA);
    let fake = Fake::new(&f.index_root, false);
    let r = f.feed(&fake).await.unwrap();
    assert_eq!(r.status, FeedStatus::Unconfirmed);
    assert_eq!((r.acknowledged, r.searchable), (1, 1));
    assert!(matches!(f.index("a", &fake), IndexState::Pending { .. }));
    assert!(!f.synced(&fake));
    fake.clear_calls();
    let r = f.feed(&fake).await.unwrap();
    assert!(fake.calls("search.index.file.put").is_empty());
    assert_eq!(r.status, FeedStatus::Unconfirmed);
}

/// Why: the chunk-coverage test (§4.9 check 2 without D1). What: a chunker
/// that drops the last paragraph fails the probe. Test: itself.
#[tokio::test]
async fn chunk_coverage_needs_every_body_character() {
    for lossy in [false, true] {
        let f = fx();
        create(&f.root, "a", &ALPHA);
        let fake = Fake::new(&f.index_root, false);
        fake.script(|s| s.lossy = lossy);
        f.feed(&fake).await.unwrap();
        let passed = f.records().iter().find_map(|r| match r {
            IndexRecord::Probe { passed, .. } => Some(*passed),
            _ => None,
        });
        assert_eq!(passed, Some(!lossy));
    }
}

/// Why: criterion 128, fake-daemon half (F16). What: a leaked
/// `_sources/registry.toml` and `_state/conversion-log.md`. Test: itself.
#[tokio::test]
async fn c128_scope_leak_is_index_scope_and_pushes_nothing() {
    let f = fx();
    create(&f.root, "a", &ALPHA);
    let fake = Fake::new(&f.index_root, true);
    fake.seed("bundle/_sources/registry.toml", "query = in:sent");
    fake.seed("bundle/_state/conversion-log.md", "conversion log");
    let got = f.feed(&fake).await;
    match &got {
        Err(OkfError::IndexScope { leaked, .. }) => assert_eq!(
            leaked,
            &vec![
                PathBuf::from("bundle/_sources/registry.toml"),
                PathBuf::from("bundle/_state/conversion-log.md")
            ]
        ),
        other => panic!("{other:?}"),
    }
    assert_eq!(exit_code(&got), 1);
    assert!(fake.calls("search.index.file.put").is_empty());
    assert_eq!(fake.calls("search.index.files.remove").len(), 2);
    assert!(f.journal_bytes().is_none());
    assert!(fake.under("bundle/_sources").is_empty());
    assert_eq!(f.feed(&fake).await.unwrap().status, FeedStatus::Searchable);
}

/// Why: a failed scope read must not let the pass push (fail closed).
/// What: chunks.list errors. Test: itself.
#[tokio::test]
async fn unreadable_scope_is_not_fed() {
    struct NoChunks(Arc<Fake>);
    #[async_trait::async_trait]
    impl IndexFeed for NoChunks {
        async fn call(&self, method: &str, params: Value) -> Result<Value, SearchError> {
            if method == "search.chunks.list" {
                return Err(SearchError {
                    method: method.into(),
                    code: Some(-32000),
                    message: "corpus read failed".into(),
                });
            }
            self.0.call(method, params).await
        }
    }
    let f = fx();
    create(&f.root, "a", &ALPHA);
    let fake = Fake::new(&f.index_root, true);
    let sink = NoChunks(fake.clone());
    let r = feed(&f.bundle(), &f.state, INDEX, &sink).await.unwrap();
    assert_eq!((r.status, r.pending), (FeedStatus::NotFed, 1));
    assert!(fake.calls("search.index.file.put").is_empty());
    assert!(f.journal_bytes().is_none());
}

/// Why: criterion 133 (row 4, case 3). What: a D1 fake that acknowledges
/// the edit and echoes the new hash but serves the old chunks and the old
/// stored hash; control: the same edit served correctly. Test: itself.
#[tokio::test]
async fn c133_old_chunks_fail_the_probe_and_the_new_ones_pass() {
    for stale in [true, false] {
        let f = fx();
        create(&f.root, "a", &ALPHA);
        let fake = Fake::new(&f.index_root, true);
        assert_eq!(f.feed(&fake).await.unwrap().status, FeedStatus::Searchable);
        assert_eq!(f.index("a", &fake), IndexState::Searchable);
        edit(
            &f.root,
            "a",
            &[ALPHA[0], ALPHA[1], "Alpha closing paragraph rewritten"],
        );
        fake.script(|s| s.stale = stale);
        let r = f.feed(&fake).await.unwrap();
        if stale {
            assert_eq!(r.status, FeedStatus::Unconfirmed, "{r:?}");
            assert_eq!(f.index("a", &fake), IndexState::Acknowledged);
            assert!(!f.synced(&fake));
        } else {
            assert_eq!(r.status, FeedStatus::Searchable, "{r:?}");
            assert_eq!(f.index("a", &fake), IndexState::Searchable);
            assert!(f.synced(&fake));
        }
    }
}

/// Why: criterion 134 (row 5, cases 1 and 3). What: feed to Searchable,
/// then re-create the index, or bump its generation; control: unchanged.
/// Test: itself.
#[tokio::test]
async fn c134_identity_change_reads_pending_and_the_next_pass_repushes() {
    for change in ["recreate", "generation", "none"] {
        let f = fx();
        create(&f.root, "a", &ALPHA);
        create(&f.root, "b", &["Beta words here"]);
        let fake = Fake::new(&f.index_root, true);
        assert_eq!(f.feed(&fake).await.unwrap().status, FeedStatus::Searchable);
        match change {
            "recreate" => fake.recreate("inst-2"),
            "generation" => fake.bump_generation(),
            _ => {}
        }
        let s = f
            .bundle()
            .state_summary(&f.state, INDEX, &f.live(&fake))
            .unwrap();
        let states: Vec<IndexState> = ["a", "b"].iter().map(|n| f.index(n, &fake)).collect();
        fake.clear_calls();
        let r = f.feed(&fake).await.unwrap();
        let puts = fake.calls("search.index.file.put").len();
        if change == "none" {
            assert_eq!((s.searchable, puts), (2, 0));
            assert!(s.synced());
            assert!(states.iter().all(|x| *x == IndexState::Searchable));
        } else {
            assert_eq!(
                (s.pending, s.acknowledged, s.searchable),
                (2, 0, 0),
                "{change}"
            );
            assert!(!s.synced());
            assert!(
                states
                    .iter()
                    .all(|x| matches!(x, IndexState::Pending { .. }))
            );
            assert_eq!(puts, 2, "{change}");
        }
        assert_eq!(r.status, FeedStatus::Searchable, "{change}");
    }
}

/// Why: criterion 134 reads Pending right after the change, before any
/// feed pass. What: the state read alone. Test: itself.
#[tokio::test]
async fn c134_new_identity_reads_pending_before_any_pass() {
    let f = fx();
    create(&f.root, "a", &ALPHA);
    let fake = Fake::new(&f.index_root, true);
    f.feed(&fake).await.unwrap();
    fake.recreate("inst-2");
    assert!(matches!(f.index("a", &fake), IndexState::Pending { .. }));
}

/// Why: criterion 69, feed half (item 22 B). What: a hit of an older
/// revision, a current hit of a verified concept, a deprecated concept and a
/// tombstoned one. Test: itself.
#[tokio::test]
async fn c69_stale_hit_is_flagged_and_withdrawn_hits_are_dropped() {
    let f = fx();
    create(&f.root, "a", &ALPHA);
    let mut b = f.bundle();
    let h = content_hash(&b.get(&id("a")).unwrap());
    let v = Verification {
        by: Actor::Process("checker".into()),
        at: now(),
        stale_after: None,
    };
    b.verify(&id("a"), v, &h).unwrap();
    std::fs::write(
        f.root.join("dep.md"),
        "---\ntype: Note\nstatus: deprecated\n---\nAlpha deprecated words\n",
    )
    .unwrap();
    std::fs::write(
        f.root.join("gone.md"),
        "---\ntype: Note\nsource_status: deleted\n---\nAlpha tombstone words\n",
    )
    .unwrap();
    let fake = Fake::new(&f.index_root, true);
    fake.seed("bundle/a.md", "Alpha old revision words no longer present");
    fake.seed("bundle/a.md", ALPHA[1]);
    fake.seed("bundle/dep.md", "Alpha deprecated words");
    fake.seed("bundle/gone.md", "Alpha tombstone words");
    let q = Query {
        text: "alpha".into(),
        top_k: 10,
    };
    let hits = retrieve(&f.bundle(), INDEX, &q, fake.as_ref(), now())
        .await
        .unwrap();
    assert_eq!(hits.len(), 2, "{hits:?}");
    assert!(hits.iter().all(|h| h.concept == id("a")));
    let stale = hits
        .iter()
        .find(|h| h.content.contains("old revision"))
        .unwrap();
    assert!(stale.stale_revision);
    assert!(matches!(
        stale.verification,
        VerificationState::Unverified { .. }
    ));
    let current = hits.iter().find(|h| !h.stale_revision).unwrap();
    assert!(matches!(
        current.verification,
        VerificationState::Verified { .. }
    ));
}

// #10873: the retrieve-hit path of D64 (`retrieve::enrich`) had no test.
/// Why: criterion 142 under owner ruling D64, retrieval half — a retrieval
/// hit reports the same verification `history` as the verification axis,
/// so a hashless event that lives only in frontmatter counts there too,
/// once, and a malformed entry does not count.
/// What: a hand-written concept with no record lines whose `verified` list
/// holds one event twice, a second event and two malformed entries; a hit
/// of an older revision and a hit of the current text both report
/// Unverified with history 2, as `verification_state` does.
/// Test: itself.
#[tokio::test]
async fn c142_retrieve_hit_counts_frontmatter_only_hashless_events() {
    let f = fx();
    std::fs::write(
        f.root.join("h.md"),
        "---\ntype: Note\nverified:\n  - { by: human:old, at: 2026-01-01T00:00:00Z }\n  \
         - { by: human:old, at: 2026-01-01T00:00:00Z }\n  \
         - { by: human:jo, at: 2026-02-01T00:00:00Z }\n  \
         - { by: human:no-at }\n  - just-a-string\n---\nAlpha hashless words here\n",
    )
    .unwrap();
    let fake = Fake::new(&f.index_root, true);
    fake.seed("bundle/h.md", "Alpha old revision words no longer present");
    fake.seed("bundle/h.md", "Alpha hashless words here");
    let q = Query {
        text: "alpha".into(),
        top_k: 10,
    };
    let hits = retrieve(&f.bundle(), INDEX, &q, fake.as_ref(), now())
        .await
        .unwrap();
    assert_eq!(hits.len(), 2, "{hits:?}");
    let want = VerificationState::Unverified { history: 2 };
    assert_eq!(f.bundle().verification_state(&id("h")).unwrap(), want);
    let stale = hits.iter().find(|h| h.stale_revision).unwrap();
    assert_eq!(stale.verification, want, "stale hit: {stale:?}");
    let current = hits.iter().find(|h| !h.stale_revision).unwrap();
    assert_eq!(current.verification, want, "current hit: {current:?}");
}

/// Why: a leaked bookkeeping file never reaches a caller (§5.3.1). What:
/// a hit under `_sources/`. Test: itself.
#[tokio::test]
async fn retrieve_refuses_a_scope_leak() {
    let f = fx();
    create(&f.root, "a", &ALPHA);
    let fake = Fake::new(&f.index_root, true);
    fake.seed("bundle/_sources/registry.toml", "alpha query = in:sent");
    let q = Query {
        text: "alpha".into(),
        top_k: 10,
    };
    let got = retrieve(&f.bundle(), INDEX, &q, fake.as_ref(), now()).await;
    assert!(matches!(got, Err(OkfError::IndexScope { .. })), "{got:?}");
    fake.script(|s| s.down = true);
    let got = retrieve(&f.bundle(), INDEX, &q, fake.as_ref(), now()).await;
    assert!(matches!(got, Err(OkfError::Search(_))), "{got:?}");
}

/// Serves `fake` over a real Unix socket with trusty-common's server.
struct Served(Arc<Fake>);

#[async_trait::async_trait]
impl trusty_common::uds::server::RpcFallback for Served {
    async fn call(
        &self,
        method: &str,
        params: Value,
    ) -> Result<Value, trusty_common::uds::server::RpcError> {
        IndexFeed::call(self.0.as_ref(), method, params)
            .await
            .map_err(|e| {
                trusty_common::uds::server::RpcError::new(e.code.unwrap_or(-32000), e.message)
            })
    }
}

/// Why: `RpcIndexFeed` speaks the daemon's socket protocol and keeps the
/// reply bodies the pass checks. What: the fake behind trusty-common's
/// server; a full pass to Searchable. Test: itself.
#[tokio::test]
async fn rpc_feed_round_trips_over_a_real_socket() {
    use trusty_common::uds::server::{RpcRouter, RpcServeOptions, serve_until};
    let f = fx();
    create(&f.root, "a", &ALPHA);
    let fake = Fake::new(&f.index_root, true);
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("search.sock");
    let listener = trusty_common::uds::bind_hardened(&socket).unwrap();
    let router = Arc::new(RpcRouter::new().fallback(Served(fake.clone())));
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        serve_until(&listener, router, RpcServeOptions::default(), async {
            let _ = rx.await;
        })
        .await;
    });
    let r = feed(&f.bundle(), &f.state, INDEX, &RpcIndexFeed::new(socket))
        .await
        .unwrap();
    let _ = tx.send(());
    server.await.unwrap();
    assert_eq!(r.status, FeedStatus::Searchable, "{r:?}");
    let put = &fake.calls("search.index.file.put")[0];
    assert_eq!(put["index_id"], INDEX);
    assert_eq!(put["body"]["path"], "bundle/a.md");
}

/// Why: F13 — a probe that cannot run leaves the put Acknowledged and the
/// pass `Unconfirmed`, with no probe record. Test: itself.
#[tokio::test]
async fn f13_probe_that_cannot_run_leaves_acknowledged() {
    let f = fx();
    create(&f.root, "a", &ALPHA);
    let fake = Fake::new(&f.index_root, true);
    fake.script(|s| s.query_error = true);
    let r = f.feed(&fake).await.unwrap();
    assert_eq!(
        (r.status, r.acknowledged, r.searchable),
        (FeedStatus::Unconfirmed, 1, 0)
    );
    assert!(r.errors[0].message.contains("probe could not run"), "{r:?}");
    assert_eq!(f.index("a", &fake), IndexState::Acknowledged);
    assert!(
        !f.records()
            .iter()
            .any(|r| matches!(r, IndexRecord::Probe { .. }))
    );
}

/// Why: a withdrawal the daemon did not confirm advances nothing (F2, F7).
/// What: a moved concept whose old path cannot be removed. Test: itself.
#[tokio::test]
async fn failed_remove_is_partial_and_keeps_the_path_owed() {
    let f = fx();
    create(&f.root, "a", &ALPHA);
    let fake = Fake::new(&f.index_root, true);
    f.feed(&fake).await.unwrap();
    f.bundle().move_concept(&id("a"), &id("b"), now()).unwrap();
    fake.script(|s| s.fail_remove = true);
    let r = f.feed(&fake).await.unwrap();
    assert_eq!(r.status, FeedStatus::Partial, "{r:?}");
    assert!(
        !f.records()
            .iter()
            .any(|r| matches!(r, IndexRecord::RemoveAck { .. }))
    );
    let backlog = f
        .bundle()
        .index_backlog(&f.state, INDEX, &f.live(&fake))
        .unwrap();
    assert_eq!(backlog.task("a.md"), Some(TaskOp::Remove));
}

/// Why: D1's revision evidence counts only under the live identity (§4.9);
/// a `file.status` reply whose identity is null (unreadable) is never a
/// match. What: a D1 fake that serves the right hash with `identity: null`.
/// Test: itself.
#[tokio::test]
async fn file_status_with_a_null_identity_is_never_searchable() {
    let f = fx();
    create(&f.root, "a", &ALPHA);
    let fake = Fake::new(&f.index_root, true);
    fake.script(|s| s.file_status_identity = Some(Value::Null));
    let r = f.feed(&fake).await.unwrap();
    assert_eq!(
        (r.status, r.searchable),
        (FeedStatus::Unconfirmed, 0),
        "{r:?}"
    );
    assert_eq!(f.index("a", &fake), IndexState::Acknowledged);
    assert!(!f.synced(&fake));
}

/// Why: as above, for a `file.status` reply naming another identity.
/// What: a D1 fake whose `file.status` names another instance. Test: itself.
#[tokio::test]
async fn file_status_with_another_identity_is_never_searchable() {
    let f = fx();
    create(&f.root, "a", &ALPHA);
    let fake = Fake::new(&f.index_root, true);
    fake.script(|s| {
        s.file_status_identity = Some(json!({"instance_id": "inst-other", "generation": 1}));
    });
    let r = f.feed(&fake).await.unwrap();
    assert_eq!(
        (r.status, r.searchable),
        (FeedStatus::Unconfirmed, 0),
        "{r:?}"
    );
    assert_eq!(f.index("a", &fake), IndexState::Acknowledged);
    assert!(!f.synced(&fake));
}

/// Why: #10876 — an offline read can only see an identity change the pass
/// journaled, and a journal must not grow on every pass.
/// What: a D1 fake fed twice, then after a generation bump and a re-create;
/// one `Observed` per distinct identity, in order. A D1-absent daemon
/// journals none. Test: itself.
#[tokio::test]
async fn observed_record_written_once_per_identity_change() {
    let f = fx();
    create(&f.root, "a", &ALPHA);
    let fake = Fake::new(&f.index_root, true);
    f.feed(&fake).await.unwrap();
    f.feed(&fake).await.unwrap();
    assert_eq!(observed(&f.records()), vec![Some(ident("inst-1", 1))]);
    fake.bump_generation();
    f.feed(&fake).await.unwrap();
    f.feed(&fake).await.unwrap();
    fake.recreate("inst-2");
    f.feed(&fake).await.unwrap();
    assert_eq!(
        observed(&f.records()),
        vec![
            Some(ident("inst-1", 1)),
            Some(ident("inst-1", 2)),
            Some(ident("inst-2", 1))
        ]
    );

    let g = fx();
    create(&g.root, "a", &ALPHA);
    let plain = Fake::new(&g.index_root, false);
    g.feed(&plain).await.unwrap();
    assert!(observed(&g.records()).is_empty());
}

/// Why: #10876 — the observation goes through the fail-closed record path
/// (F3): a pass that cannot journal the new identity must not run.
/// What: after a settled pass, the generation bumps and the journal turns
/// read-only. The pass is `JournalWrite`, sends no put or remove, and
/// leaves the journal unchanged. Test: itself.
#[tokio::test]
async fn observed_write_failure_is_not_fed() {
    use std::os::unix::fs::PermissionsExt;
    let f = fx();
    create(&f.root, "a", &ALPHA);
    let fake = Fake::new(&f.index_root, true);
    assert_eq!(f.feed(&fake).await.unwrap().status, FeedStatus::Searchable);
    fake.bump_generation();
    fake.script(|s| {
        s.fail_put.insert("bundle/a.md".into());
    });
    let jpath = f.journal().path().to_path_buf();
    let before = f.journal_bytes();
    std::fs::set_permissions(&jpath, std::fs::Permissions::from_mode(0o444)).unwrap();
    fake.clear_calls();
    let got = f.feed(&fake).await;
    std::fs::set_permissions(&jpath, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(got, Err(OkfError::JournalWrite { .. })), "{got:?}");
    assert_eq!(exit_code(&got), 1);
    assert!(fake.calls("search.index.file.put").is_empty());
    assert!(fake.calls("search.index.file.remove").is_empty());
    assert_eq!(f.journal_bytes(), before);
}

/// Why: #10876 — a daemon rolled back to a build without D1 must not leave
/// the offline read comparing new identity-less acks against the stale
/// identity, which owes a re-push no pass sends.
/// What: a D1 pass, then a pass with the daemon down (Unreadable: no
/// record), then two passes with D1 gone and an edited concept. Exactly one
/// `Observed { None }` is appended, the offline backlog owes nothing and
/// equals the `Unreadable` backlog, and D1's return journals the identity
/// again. Test: itself.
#[tokio::test]
async fn d1_rollback_journals_one_absent_observation() {
    let f = fx();
    create(&f.root, "a", &ALPHA);
    let fake = Fake::new(&f.index_root, true);
    assert_eq!(f.feed(&fake).await.unwrap().status, FeedStatus::Searchable);
    assert_eq!(observed(&f.records()), vec![Some(ident("inst-1", 1))]);

    fake.script(|s| s.down = true);
    let before = f.journal_bytes();
    f.feed(&fake).await.unwrap();
    assert_eq!(f.journal_bytes(), before, "Unreadable writes nothing");
    fake.script(|s| s.down = false);

    fake.drop_d1();
    edit(&f.root, "a", &["Alpha words edited after the rollback"]);
    f.feed(&fake).await.unwrap();
    f.feed(&fake).await.unwrap();
    assert_eq!(
        observed(&f.records()),
        vec![Some(ident("inst-1", 1)), None],
        "one absent observation, none on Unsupported -> Unsupported"
    );
    let offline = f.bundle().index_backlog_offline(&f.state, INDEX).unwrap();
    assert_eq!(offline.owed(), 0, "{offline:?}");
    let down = LiveIdentity::Unreadable("down".into());
    assert_eq!(
        offline,
        f.bundle().index_backlog(&f.state, INDEX, &down).unwrap()
    );

    fake.recreate("inst-2");
    f.feed(&fake).await.unwrap();
    assert_eq!(
        observed(&f.records()),
        vec![Some(ident("inst-1", 1)), None, Some(ident("inst-2", 1))]
    );
}

/// Why: #10876 criterion — a journal with no `Observed` record (D1 absent,
/// or written before #10876) reads exactly as before.
/// What: after a D1-absent pass and an edit, and for a hand-written
/// pre-#10876 journal whose acks name an identity, the offline backlog
/// equals the backlog under `Unsupported` and under `Unreadable`.
/// Test: itself.
#[tokio::test]
async fn index_backlog_offline_equals_index_backlog_when_no_identity() {
    let f = fx();
    create(&f.root, "a", &ALPHA);
    create(&f.root, "b", &["Beta words here"]);
    f.feed(&Fake::new(&f.index_root, false)).await.unwrap();
    edit(&f.root, "b", &["Beta words changed"]);
    create(&f.root, "c", &["Gamma words"]);

    let g = fx();
    create(&g.root, "a", &ALPHA);
    let bytes = trusty_okf::format::hash::bytes_hash(&std::fs::read(g.root.join("a.md")).unwrap());
    g.journal()
        .append(&[
            IndexRecord::PutAck {
                path: "a.md".into(),
                bytes: bytes.clone(),
                identity: Some(ident("inst-1", 1)),
            },
            IndexRecord::Probe {
                path: "a.md".into(),
                bytes,
                identity: Some(ident("inst-1", 1)),
                passed: true,
            },
        ])
        .unwrap();

    for (x, owed) in [(&f, 2), (&g, 0)] {
        let offline = x.bundle().index_backlog_offline(&x.state, INDEX).unwrap();
        assert_eq!(offline.owed(), owed, "{offline:?}");
        for live in [
            LiveIdentity::Unsupported,
            LiveIdentity::Unreadable("down".into()),
        ] {
            let online = x.bundle().index_backlog(&x.state, INDEX, &live).unwrap();
            assert_eq!(offline, online, "{live:?}");
        }
    }
}

/// Why: #10876 — the new record kind is additive. What: `Observed`
/// round-trips through its JSON line, and a reader built without it (an
/// older binary) skips the line as malformed with no error. Test: itself.
#[test]
fn observed_record_round_trips_and_older_readers_skip_it() {
    let r = IndexRecord::Observed {
        identity: Some(ident("inst-1", 3)),
    };
    let line = serde_json::to_string(&r).unwrap();
    // #10876: the bare-object form the first #10876 commit wrote parses as
    // `Some`, and a D1-absent observation round-trips as `null`.
    assert_eq!(
        line,
        r#"{"kind":"observed","identity":{"instance_id":"inst-1","generation":3}}"#
    );
    assert_eq!(serde_json::from_str::<IndexRecord>(&line).unwrap(), r);
    let absent = IndexRecord::Observed { identity: None };
    let null_line = serde_json::to_string(&absent).unwrap();
    assert_eq!(null_line, r#"{"kind":"observed","identity":null}"#);
    assert_eq!(
        serde_json::from_str::<IndexRecord>(&null_line).unwrap(),
        absent
    );

    #[derive(serde::Deserialize, Debug)]
    #[serde(tag = "kind", rename_all = "snake_case")]
    #[allow(dead_code)]
    enum Older {
        Reindex { identity: Option<IndexIdentity> },
    }
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("j.jsonl");
    std::fs::write(
        &path,
        format!("{{\"kind\":\"reindex\",\"identity\":null}}\n{line}\n"),
    )
    .unwrap();
    let load = trusty_okf::build::jsonl::read_journal::<Older>(&path).unwrap();
    assert_eq!((load.records.len(), load.malformed), (1, 1));
}
