//! Safety tests for the `import kuzu` round-1 critic fixes (#277).
//!
//! One test per finding (H1-H4, M1-M3, M6, L1, L2, L5, L6); each was run red
//! against its reverted decision before it was committed. Fixtures, fakes and
//! the palace helpers come from `tests.rs`. Synthetic data only.

use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::{json, Value};
use trusty_common::memory_core::filter::check_secret;
use trusty_common::memory_core::palace::Drawer;
use trusty_common::memory_core::store::Triple;
use trusty_common::memory_core::PalaceHandle;
use uuid::Uuid;

use super::apply::{HandleSink, PalaceSink, PalaceView, StoreCounts};
use super::bridge::{export_store, resolve_python, CommandRunner, RunOutput, SystemRunner};
use super::discovery::{resolve_from, DiscoveredStore};
use super::mapping::MappedMemory;
use super::palace_io::open_palace_for_write;
use super::report::format_report;
use super::tests::{
    args_for, env, export_of, fixture, memory_row, palace, palace_state, store_on_disk, FakeDaemon,
    FakeRunner, FIXTURE_TRIPLES,
};
use super::*;

// ── helpers ─────────────────────────────────────────────────────────────

/// `created_at` of every fixture row, as a Unix timestamp.
const FIXTURE_CREATED_AT: i64 = 1_762_165_331;

/// Write `v` into `sink` as if it were read from `store`.
async fn run_from(
    sink: &dyn PalaceSink,
    v: &Value,
    store: &DiscoveredStore,
    update: bool,
) -> StoreCounts {
    let tag = store.dir.to_string_lossy();
    run_plan(&export_of(v), &tag, Target::Write(sink), update).await
}

/// The drawer carrying `tag`, from the handle's in-memory table.
fn drawer_tagged(h: &PalaceHandle, tag: &str) -> Option<Drawer> {
    h.drawers
        .read()
        .iter()
        .find(|d| d.tags.iter().any(|t| t == tag))
        .cloned()
}

/// Every path under `dir`, relative to it, sorted.
fn listing(dir: &Path) -> BTreeSet<PathBuf> {
    let mut out = BTreeSet::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).expect("read_dir").flatten() {
            let p = entry.path();
            out.insert(p.strip_prefix(dir).expect("under dir").to_path_buf());
            if entry.file_type().expect("type").is_dir() {
                stack.push(p);
            }
        }
    }
    out
}

/// Block until nothing holds `redb`'s file lock on `path` (5 s bound).
///
/// Why: a dropped handle's KG writer task is aborted, not joined, and redb
/// writes its clean-shutdown header when that last reference drops. A test
/// that snapshots `kg.redb` bytes must wait for that write, and must not open
/// the database to find out, because an open writes too.
fn wait_until_closed(path: &Path) {
    use std::os::fd::AsRawFd;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let file = std::fs::File::open(path).expect("open for lock probe");
        // SAFETY: `file` owns a valid fd for the duration of both calls.
        let locked = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0;
        if !locked {
            // SAFETY: as above; releases the probe's own lock.
            unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
            return;
        }
        assert!(Instant::now() < deadline, "{} stayed open", path.display());
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A store report with every field chosen by the caller.
fn report(status: StoreStatus, counts: StoreCounts, flush: Option<&str>) -> StoreReport {
    StoreReport {
        store: PathBuf::from("/work/proj/.kuzu-memory"),
        palace: Some("proj-palace".to_string()),
        source: Some("pin file"),
        counts,
        status,
        flush_error: flush.map(str::to_string),
    }
}

/// Sets one environment variable and restores the prior value on drop.
struct EnvGuard {
    key: &'static str,
    prev: Option<OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let prev = std::env::var_os(key);
        // SAFETY: the caller is `#[serial_test::serial]`, so no sibling test
        // reads or writes the process environment concurrently.
        unsafe { std::env::set_var(key, value) };
        Self { key, prev }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: see `set`.
        unsafe {
            match self.prev.take() {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

/// A [`PalaceSink`] over a real handle that can fail chosen calls and records
/// the tags a drawer carries right after its insert.
struct ScriptedSink {
    inner: HandleSink,
    /// Fail the n-th `stamp_drawer` call (1-based); 0 never fails.
    fail_stamp_on: usize,
    /// Fail the n-th `assert_triple` call (1-based); 0 never fails.
    fail_triple_on: usize,
    stamps: Mutex<usize>,
    triples: Mutex<usize>,
    /// `(call, memory id)` in call order.
    calls: Mutex<Vec<(&'static str, String)>>,
    /// Tags of each drawer as `insert_drawer` left it.
    inserted_tags: Mutex<Vec<Vec<String>>>,
}

impl ScriptedSink {
    fn new(handle: &Arc<PalaceHandle>, fail_stamp_on: usize, fail_triple_on: usize) -> Self {
        Self {
            inner: HandleSink {
                handle: Arc::clone(handle),
            },
            fail_stamp_on,
            fail_triple_on,
            stamps: Mutex::new(0),
            triples: Mutex::new(0),
            calls: Mutex::new(Vec::new()),
            inserted_tags: Mutex::new(Vec::new()),
        }
    }

    fn log(&self, call: &'static str, m: &MappedMemory) {
        let mut calls = self.calls.lock().expect("lock");
        calls.push((call, m.memory_id.clone()));
    }
}

/// Bump `counter` and report whether this call is the `fail_on`-th.
fn nth(counter: &Mutex<usize>, fail_on: usize) -> bool {
    let mut n = counter.lock().expect("lock");
    *n += 1;
    *n == fail_on
}

#[async_trait]
impl PalaceView for ScriptedSink {
    fn drawers(&self) -> Vec<Drawer> {
        self.inner.drawers()
    }
    async fn triple_is_active(&self, t: &Triple) -> Result<bool, KuzuImportError> {
        self.inner.triple_is_active(t).await
    }
}

#[async_trait]
impl PalaceSink for ScriptedSink {
    async fn insert_drawer(&self, m: &MappedMemory) -> Result<Uuid, KuzuImportError> {
        self.log("insert", m);
        let id = self.inner.insert_drawer(m).await?;
        let tags = self
            .inner
            .handle
            .drawers
            .read()
            .iter()
            .find(|d| d.id == id)
            .map(|d| d.tags.clone())
            .unwrap_or_default();
        self.inserted_tags.lock().expect("lock").push(tags);
        Ok(id)
    }
    async fn stamp_drawer(&self, id: Uuid, m: &MappedMemory) -> Result<(), KuzuImportError> {
        self.log("stamp", m);
        if nth(&self.stamps, self.fail_stamp_on) {
            return Err(KuzuImportError::Palace(
                "injected stamp failure".to_string(),
            ));
        }
        self.inner.stamp_drawer(id, m).await
    }
    async fn update_memory(&self, id: Uuid, m: &MappedMemory) -> Result<(), KuzuImportError> {
        self.log("update", m);
        self.inner.update_memory(id, m).await
    }
    async fn assert_triple(&self, t: Triple) -> Result<(), KuzuImportError> {
        if nth(&self.triples, self.fail_triple_on) {
            return Err(KuzuImportError::Palace(
                "injected triple failure".to_string(),
            ));
        }
        self.inner.assert_triple(t).await
    }
}

// ── H1: only a genuinely absent palace is created ───────────────────────

/// Why (#277 H1): an existing palace whose `palace.json` does not decode must
/// fail the store, never be re-created over the top of the operator's data.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn corrupt_palace_json_fails_the_store_and_is_left_untouched() {
    let tmp = tempfile::tempdir().expect("tmp");
    let data_root = tmp.path().join("data");
    drop(palace(&data_root, "kuzu-h1"));
    let meta = data_root.join("kuzu-h1").join("palace.json");
    std::fs::write(&meta, b"{ this is not palace metadata").expect("corrupt");
    let before = std::fs::read(&meta).expect("read");

    let store = store_on_disk(tmp.path(), "proj");
    let runner = FakeRunner::writing(&fixture());
    let daemon = FakeDaemon(None);
    let r = import_one(
        &env(&runner, &daemon, &data_root),
        &store,
        &args_for(&store, "kuzu-h1", false),
    )
    .await;
    assert!(
        matches!(r.status, StoreStatus::Failed(KuzuImportError::Palace(_))),
        "a corrupt palace.json fails the store: {:?}",
        r.status
    );
    assert_eq!(
        std::fs::read(&meta).expect("read"),
        before,
        "palace.json bytes must be unchanged"
    );
    // A genuinely absent palace is still created.
    open_palace_for_write(&data_root, "kuzu-h1-new").expect("absent palace is created");
    assert!(data_root.join("kuzu-h1-new").join("palace.json").exists());
}

// ── H2: a walk refuses TRUSTY_MEMORY_PALACE; --from honours it ──────────

/// Why (#277 H2): every tm session exports `TRUSTY_MEMORY_PALACE`, so a walk
/// run from a session would pour every store into one palace. `--from` still
/// honours it, and each store line names the palace and the rule behind it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn walk_refuses_an_env_palace_and_from_reports_its_source() {
    let tmp = tempfile::tempdir().expect("tmp");
    let data_root = tmp.path().join("data");
    let store = store_on_disk(tmp.path(), "proj");
    let runner = FakeRunner::writing(&fixture());
    let daemon = FakeDaemon(None);
    let session = "kuzu-env-session";
    let mut with_env = env(&runner, &daemon, &data_root);
    with_env.env_palace = Some(session.to_string());

    for walk in [
        KuzuImportArgs {
            discover: true,
            ..KuzuImportArgs::default()
        },
        KuzuImportArgs {
            root: vec![tmp.path().to_path_buf()],
            dry_run: true,
            ..KuzuImportArgs::default()
        },
    ] {
        let err = run_import(&walk, std::slice::from_ref(&store), &with_env)
            .await
            .expect_err("a walk refuses while TRUSTY_MEMORY_PALACE is set");
        assert!(
            matches!(&err, KuzuImportError::EnvPalaceWithWalk(v) if v == session),
            "{err}"
        );
    }
    assert!(runner.temp_dir().is_none(), "no store was exported");
    assert!(!data_root.exists(), "no palace was touched");

    // `--from` honours the variable, read by the resolver from the process.
    let _guard = EnvGuard::set("TRUSTY_MEMORY_PALACE", session);
    let from = KuzuImportArgs {
        from: Some(store.dir.clone()),
        dry_run: true,
        ..KuzuImportArgs::default()
    };
    let reports = run_import(&from, std::slice::from_ref(&store), &with_env)
        .await
        .expect("--from runs with the variable set");
    let r = &reports[0];
    assert_eq!(r.palace.as_deref(), Some(session));
    assert_eq!(r.source, Some("TRUSTY_MEMORY_PALACE"));
    assert!(
        matches!(r.status, StoreStatus::WouldImport),
        "{:?}",
        r.status
    );
    let line = format_report(r, false);
    assert!(
        line.contains(&format!("-> {session} (TRUSTY_MEMORY_PALACE)")),
        "{line}"
    );
}

// ── H3: a degraded drawer load refuses the write and the dry run ────────

/// Inject one undecodable `DRAWERS` row into `palace`'s closed `kg.redb`.
///
/// The dropped handle's KG writer task is aborted, not joined, so its lock
/// can outlive the drop briefly; the open is retried for up to 5 s.
fn corrupt_one_drawer_row(data_root: &Path, palace: &str) {
    use trusty_common::memory_core::store::kg_store::DRAWERS;
    let path = data_root.join(palace).join("kg.redb");
    let deadline = Instant::now() + Duration::from_secs(5);
    let db = loop {
        match redb::Database::create(&path) {
            Ok(db) => break db,
            Err(redb::DatabaseError::DatabaseAlreadyOpen) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => panic!("open kg.redb: {e}"),
        }
    };
    let wtx = db.begin_write().expect("begin write");
    {
        let mut table = wtx.open_table(DRAWERS).expect("drawers table");
        let key = Uuid::new_v4().into_bytes();
        table
            .insert(key.as_slice(), [0xFFu8; 4].as_slice())
            .expect("insert bad row");
    }
    wtx.commit().expect("commit");
}

/// Why (#277 H3): the drawers are the import ledger. A palace whose drawer
/// table loaded with unreadable rows would re-import every memory it lost,
/// so both the write and the dry run refuse it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn degraded_drawer_load_refuses_the_write_and_the_dry_run() {
    let tmp = tempfile::tempdir().expect("tmp");
    let data_root = tmp.path().join("data");
    {
        let sink = HandleSink {
            handle: palace(&data_root, "kuzu-h3"),
        };
        run_plan(
            &export_of(&fixture()),
            "store1",
            Target::Write(&sink),
            false,
        )
        .await;
    }
    corrupt_one_drawer_row(&data_root, "kuzu-h3");

    let store = store_on_disk(tmp.path(), "proj");
    let runner = FakeRunner::writing(&fixture());
    let daemon = FakeDaemon(None);
    let e = env(&runner, &daemon, &data_root);
    let write = import_one(&e, &store, &args_for(&store, "kuzu-h3", false)).await;
    assert!(
        matches!(write.status, StoreStatus::Failed(KuzuImportError::DrawersUnreadable(ref p)) if p == "kuzu-h3"),
        "write: {:?}",
        write.status
    );
    let dry = import_one(&e, &store, &args_for(&store, "kuzu-h3", true)).await;
    assert!(
        matches!(dry.status, StoreStatus::Failed(KuzuImportError::DrawersUnreadable(ref p)) if p == "kuzu-h3"),
        "dry run: {:?}",
        dry.status
    );
}

// ── H4: identity is Memory.id; shared-id rules ──────────────────────────

/// Why (#277 H4): identity is `source:kuzu-memory/<Memory.id>` with the store
/// path as provenance only, so a moved store maps onto what it imported.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn moved_store_reimports_nothing_into_the_palace() {
    let tmp = tempfile::tempdir().expect("tmp");
    let sink = HandleSink {
        handle: palace(tmp.path(), "kuzu-h4-move"),
    };
    let store = store_on_disk(tmp.path(), "proj");
    let first = run_from(&sink, &fixture(), &store, false).await;
    assert_eq!(
        (first.new_memories, first.new_triples),
        (3, FIXTURE_TRIPLES)
    );
    let before = palace_state(&sink.handle);

    std::fs::rename(tmp.path().join("proj"), tmp.path().join("proj-moved")).expect("move");
    let moved = resolve_from(&tmp.path().join("proj-moved/.kuzu-memory")).expect("moved");
    assert_ne!(moved.dir, store.dir);
    let second = run_from(&sink, &fixture(), &moved, false).await;
    assert_eq!(
        (second.new_memories, second.new_triples, second.unchanged),
        (0, 0, 3),
        "{second:?}"
    );
    assert_eq!(
        palace_state(&sink.handle),
        before,
        "nothing new was written"
    );
    // The identity is the Memory.id; the original store path is provenance.
    let d = drawer_tagged(&sink.handle, "source:kuzu-memory/m-1").expect("identity tag");
    let provenance = format!("kuzu-store:{}", store.dir.display());
    assert!(d.tags.contains(&provenance), "{:?}", d.tags);
}

/// Why (#277 H4): one `Memory.id` held by two stores. Same content from a
/// copy is unchanged; different content while the recorded store still
/// exists is skipped and reported; once the recorded store is gone, the
/// difference is a change that needs `--update`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn moved_store_reimports_nothing_and_shared_ids_are_reported() {
    let tmp = tempfile::tempdir().expect("tmp");
    let sink = HandleSink {
        handle: palace(tmp.path(), "kuzu-h4-shared"),
    };
    let a = store_on_disk(tmp.path(), "proj-a");
    let b = store_on_disk(tmp.path(), "proj-b");
    run_from(&sink, &fixture(), &a, false).await;
    let before = palace_state(&sink.handle);

    // (a) same hash from another store: unchanged.
    let copy = run_from(&sink, &fixture(), &b, false).await;
    assert_eq!(
        (copy.unchanged, copy.new_memories, copy.shared_ids.len()),
        (3, 0, 0),
        "{copy:?}"
    );

    // (b) different hash while the recorded store exists: skipped, reported.
    let mut other = fixture();
    other["memories"][1]["content"] = json!("synthetic note about a different gadget order");
    other["memories"][1]["content_hash"] = json!("h2-other");
    let shared = run_from(&sink, &other, &b, true).await;
    assert_eq!(shared.shared_ids, vec!["m-2".to_string()], "{shared:?}");
    assert_eq!(
        (
            shared.changed,
            shared.updated,
            shared.skipped_edges,
            shared.failed_writes
        ),
        (0, 0, 1, 0),
        "m-2's MENTIONS edge is skipped: {shared:?}"
    );
    let m2 = drawer_tagged(&sink.handle, "source:kuzu-memory/m-2").expect("m-2");
    assert!(
        m2.tags.contains(&"kuzu-hash:h2".to_string()),
        "not overwritten"
    );
    assert_eq!(palace_state(&sink.handle), before);

    // (c) the recorded store is gone: a change that needs --update.
    std::fs::remove_dir_all(tmp.path().join("proj-a")).expect("remove store a");
    let changed = run_from(&sink, &other, &b, false).await;
    assert_eq!(
        (changed.changed, changed.updated, changed.shared_ids.len()),
        (1, 0, 0),
        "{changed:?}"
    );
    let line = format_report(&report(StoreStatus::UpToDate, changed, None), false);
    assert!(line.contains("re-run with --update"), "{line}");
}

// ── M1: the dry run leaves an existing palace byte-identical ────────────

/// Why (#277 M1): opening the live `kg.redb`, even read-only, can write to
/// it; the dry run reads a temp copy, so the palace is left byte-identical.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dry_run_through_import_one_leaves_the_palace_byte_identical() {
    let tmp = tempfile::tempdir().expect("tmp");
    let data_root = tmp.path().join("data");
    {
        let sink = HandleSink {
            handle: palace(&data_root, "kuzu-m1"),
        };
        run_plan(
            &export_of(&fixture()),
            "store1",
            Target::Write(&sink),
            false,
        )
        .await;
        sink.handle.flush().expect("flush");
    }
    let dir = data_root.join("kuzu-m1");
    let kg = dir.join("kg.redb");
    wait_until_closed(&kg);
    let bytes = std::fs::read(&kg).expect("kg bytes");
    let mtime = std::fs::metadata(&kg)
        .expect("meta")
        .modified()
        .expect("mtime");
    let files = listing(&dir);
    // Let a rewrite, if one happens, land on a later mtime.
    tokio::time::sleep(Duration::from_millis(20)).await;

    let mut more = fixture();
    more["memories"]
        .as_array_mut()
        .expect("array")
        .push(memory_row(
            "m-4",
            "synthetic note about sprocket sizes",
            "h4",
        ));
    let store = store_on_disk(tmp.path(), "proj");
    let runner = FakeRunner::writing(&more);
    let daemon = FakeDaemon(None);
    let r = import_one(
        &env(&runner, &daemon, &data_root),
        &store,
        &args_for(&store, "kuzu-m1", true),
    )
    .await;
    assert!(
        matches!(r.status, StoreStatus::WouldImport),
        "{:?}",
        r.status
    );
    assert_eq!(
        (r.counts.new_memories, r.counts.unchanged),
        (1, 3),
        "the dry run read the existing drawers"
    );
    assert_eq!(
        std::fs::read(&kg).expect("kg bytes"),
        bytes,
        "kg.redb bytes"
    );
    assert_eq!(
        std::fs::metadata(&kg)
            .expect("meta")
            .modified()
            .expect("mtime"),
        mtime,
        "kg.redb mtime"
    );
    assert_eq!(listing(&dir), files, "palace directory listing");
}

// ── M2 / M3: pending drawers and partial writes ─────────────────────────

/// Why (#277 M2): a drawer is inserted with a pending marker and stamped with
/// identity, hash and `created_at` in one write. A run that dies between the
/// two leaves a pending drawer, and the next run finishes that drawer rather
/// than inserting a second copy.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stamp_failure_leaves_a_pending_drawer_the_next_run_finishes() {
    let tmp = tempfile::tempdir().expect("tmp");
    let handle = palace(tmp.path(), "kuzu-m2");
    let first_sink = ScriptedSink::new(&handle, 1, 0);
    let first = write_run_via(&first_sink).await;
    assert_eq!(
        (first.new_memories, first.failed_writes),
        (2, 3),
        "{first:?}"
    );

    // Each insert carried only the pending marker, never the identity.
    for tags in first_sink.inserted_tags.lock().expect("lock").iter() {
        assert!(
            tags.iter().any(|t| t.starts_with("kuzu-pending:")),
            "{tags:?}"
        );
        assert!(
            !tags
                .iter()
                .any(|t| t.starts_with("source:") || t.starts_with("kuzu-hash:")),
            "{tags:?}"
        );
    }
    // m-2 went insert -> stamp: one write stamps everything.
    let m2_calls: Vec<&str> = first_sink
        .calls
        .lock()
        .expect("lock")
        .iter()
        .filter(|(_, id)| id == "m-2")
        .map(|(c, _)| *c)
        .collect();
    assert_eq!(m2_calls, vec!["insert", "stamp"]);
    let m2 = drawer_tagged(&handle, "source:kuzu-memory/m-2").expect("m-2 stamped");
    assert!(m2.tags.contains(&"kuzu-hash:h2".to_string()));
    assert!(!m2.tags.iter().any(|t| t.starts_with("kuzu-pending:")));
    assert_eq!(m2.created_at.timestamp(), FIXTURE_CREATED_AT);

    let pending = drawer_tagged(&handle, "kuzu-pending:m-1").expect("m-1 left pending");
    assert!(drawer_tagged(&handle, "source:kuzu-memory/m-1").is_none());
    assert_eq!(palace_state(&handle).0, 3);

    let second_sink = ScriptedSink::new(&handle, 0, 0);
    let second = write_run_via(&second_sink).await;
    assert_eq!(
        (second.new_memories, second.unchanged, second.failed_writes),
        (1, 2, 0),
        "{second:?}"
    );
    let finished = drawer_tagged(&handle, "source:kuzu-memory/m-1").expect("m-1 finished");
    assert_eq!(finished.id, pending.id, "the pending drawer, not a new one");
    assert!(!finished.tags.iter().any(|t| t.starts_with("kuzu-pending:")));
    assert!(finished.tags.contains(&"kuzu-hash:h1".to_string()));
    assert_eq!(finished.created_at.timestamp(), FIXTURE_CREATED_AT);
    let (drawers, active, _) = palace_state(&handle);
    assert_eq!((drawers, active), (3, FIXTURE_TRIPLES));
}

/// Run the fixture through `sink` as store `store1`.
async fn write_run_via(sink: &ScriptedSink) -> StoreCounts {
    run_plan(&export_of(&fixture()), "store1", Target::Write(sink), false).await
}

/// Why (#277 M3): a store whose drawer landed but whose stamp failed, or
/// whose triple assert failed, is reported Partial — never done — and the
/// next run completes it without a duplicate drawer or triple row.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stamp_or_triple_failure_is_partial_and_the_rerun_completes_it() {
    for (case, fail_stamp_on, fail_triple_on) in [("stamp", 1, 0), ("triple", 0, 1)] {
        let tmp = tempfile::tempdir().expect("tmp");
        let handle = palace(tmp.path(), "kuzu-m3");
        let first = write_run_via(&ScriptedSink::new(&handle, fail_stamp_on, fail_triple_on)).await;
        assert!(first.failed_writes > 0, "{case}: {first:?}");
        assert!(
            matches!(status_for(&first, false), StoreStatus::Partial),
            "{case}: a failed write is Partial"
        );
        let r = report(status_for(&first, false), first, None);
        assert!(r.is_bad(), "{case}: Partial makes the run exit non-zero");

        let second = write_run_via(&ScriptedSink::new(&handle, 0, 0)).await;
        assert_eq!(second.failed_writes, 0, "{case}: {second:?}");
        assert!(
            matches!(status_for(&second, false), StoreStatus::Imported),
            "{case}"
        );
        assert_eq!(
            palace_state(&handle),
            (3, FIXTURE_TRIPLES, FIXTURE_TRIPLES),
            "{case}: no duplicate drawer or triple row"
        );
        let third = write_run_via(&ScriptedSink::new(&handle, 0, 0)).await;
        assert!(
            matches!(status_for(&third, false), StoreStatus::UpToDate),
            "{case}"
        );
    }
}

// ── M6: secret-shaped memories are refused ──────────────────────────────

/// Why (#277 M6): a memory the palace's secret screen rejects is refused and
/// its id reported; the store does not fail, and edges touching it are
/// skipped rather than dangling.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn secret_shaped_memory_is_refused_without_failing_the_store() {
    // Built at run time so no credential-shaped literal sits in the source.
    let token = format!("{}-test-{}", "sk", "FAKEfake0123456789abcdefXYZ");
    let content = format!("synthetic note carrying {token} by mistake");
    assert!(check_secret(&content).is_err(), "the screen rejects it");

    let tmp = tempfile::tempdir().expect("tmp");
    let sink = HandleSink {
        handle: palace(tmp.path(), "kuzu-m6"),
    };
    let mut v = fixture();
    v["memories"][1]["content"] = json!(content);
    let c = run_plan(&export_of(&v), "store1", Target::Write(&sink), false).await;
    assert_eq!(c.refused_ids, vec!["m-2".to_string()], "{c:?}");
    assert_eq!(
        (
            c.new_memories,
            c.failed_writes,
            c.skipped_edges,
            c.new_triples
        ),
        (2, 0, 1, FIXTURE_TRIPLES - 1),
        "{c:?}"
    );
    assert!(matches!(status_for(&c, false), StoreStatus::Imported));
    assert_eq!(palace_state(&sink.handle).0, 2);
    assert!(sink
        .handle
        .drawers
        .read()
        .iter()
        .all(|d| !d.content().contains(&token)));
    let line = format_report(&report(StoreStatus::Imported, c, None), false);
    assert!(
        line.contains("refused 1 secret-shaped memory(ies): m-2"),
        "{line}"
    );
    assert!(!line.contains(&token), "the report carries the id only");
}

// ── L1 / L2: the bridge process ─────────────────────────────────────────

/// Runs every command as `/bin/sh -c <script>` under a [`SystemRunner`].
struct ShellRunner {
    inner: SystemRunner,
    script: String,
}

impl CommandRunner for ShellRunner {
    fn run(&self, _program: &Path, _args: &[OsString]) -> std::io::Result<RunOutput> {
        let args = [OsString::from("-c"), OsString::from(&self.script)];
        self.inner.run(Path::new("/bin/sh"), &args)
    }
}

/// Why (#277 L1): a wedged interpreter must not hang the run; past its bound
/// the child is killed and reaped, and the store fails as timed out.
#[test]
fn system_runner_kills_a_child_past_its_timeout() {
    let tmp = tempfile::tempdir().expect("tmp");
    let pid_file = tmp.path().join("pid");
    let runner = ShellRunner {
        inner: SystemRunner::new(Duration::from_secs(1)),
        script: format!("echo $$ > '{}'; exec sleep 10", pid_file.display()),
    };
    let started = Instant::now();
    let err = export_store(&runner, Path::new("python3"), Path::new("memories.db"))
        .expect_err("the bridge times out");
    let elapsed = started.elapsed();
    assert!(matches!(err, KuzuImportError::BridgeTimedOut(_)), "{err:?}");
    assert!(
        elapsed < Duration::from_secs(5),
        "killed at the bound: {elapsed:?}"
    );
    let pid: i32 = std::fs::read_to_string(&pid_file)
        .expect("pid file")
        .trim()
        .parse()
        .expect("pid");
    // SAFETY: signal 0 only checks that the pid exists.
    let alive = unsafe { libc::kill(pid, 0) } == 0;
    assert!(!alive, "child {pid} was killed and reaped");
}

/// Why (#277 L2): a pip `#!/bin/sh` trampoline, or a shebang path with a
/// space cut short by whitespace-splitting, names no python; the error says
/// to pass `--python`.
#[test]
fn non_python_shebang_is_refused_with_the_python_hint() {
    let tmp = tempfile::tempdir().expect("tmp");
    let path_var = OsString::from(tmp.path());
    for shebang in ["#!/bin/sh", "#!/opt/my tools/bin/python3"] {
        std::fs::write(
            tmp.path().join("kuzu-memory"),
            format!("{shebang}\nexec x\n"),
        )
        .expect("launcher");
        let err = resolve_python(None, Some(OsStr::new(&path_var))).expect_err(shebang);
        let msg = err.to_string();
        assert!(
            matches!(err, KuzuImportError::InterpreterNotFound(_)),
            "{shebang}: {err:?}"
        );
        assert!(
            msg.contains("not a python interpreter") && msg.contains("pass --python"),
            "{shebang}: {msg}"
        );
    }
}

// ── L5: --update keeps tags the importer did not generate ───────────────

/// Why (#277 L5): `--update` replaces the importer's own tags and keeps every
/// tag something else added to the drawer since.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn update_keeps_tags_the_importer_did_not_generate() {
    let tmp = tempfile::tempdir().expect("tmp");
    let sink = HandleSink {
        handle: palace(tmp.path(), "kuzu-l5"),
    };
    run_plan(
        &export_of(&fixture()),
        "store1",
        Target::Write(&sink),
        false,
    )
    .await;
    let mut d = drawer_tagged(&sink.handle, "source:kuzu-memory/m-2").expect("m-2");
    d.tags.push("starred".to_string());
    d.tags.push("topic:gadgets".to_string());
    sink.handle.kg.upsert_drawer(&d).await.expect("persist");
    if let Some(slot) = sink
        .handle
        .drawers
        .write()
        .iter_mut()
        .find(|x| x.id == d.id)
    {
        *slot = d.clone();
    }

    let mut edited = fixture();
    edited["memories"][1]["content"] = json!("synthetic note about the revised gadget order");
    edited["memories"][1]["content_hash"] = json!("h2-new");
    edited["memories"][1]["memory_type"] = json!("episodic");
    let c = run_plan(&export_of(&edited), "store1", Target::Write(&sink), true).await;
    assert_eq!((c.changed, c.updated, c.failed_writes), (1, 1, 0), "{c:?}");
    let after = sink
        .handle
        .kg
        .load_drawer(d.id)
        .expect("load")
        .expect("row");
    for kept in ["starred", "topic:gadgets"] {
        assert!(
            after.tags.contains(&kept.to_string()),
            "{kept}: {:?}",
            after.tags
        );
    }
    assert!(after.tags.contains(&"kuzu-hash:h2-new".to_string()));
    assert!(after.tags.contains(&"memory_type:episodic".to_string()));
    for gone in ["kuzu-hash:h2", "memory_type:semantic"] {
        assert!(
            !after.tags.contains(&gone.to_string()),
            "{gone}: {:?}",
            after.tags
        );
    }
}

// ── L6 / H2: the store line ─────────────────────────────────────────────

/// Why (#277 L6, H2, M6, H4): the store line is the operator's only view of a
/// run. It names the palace and the rule that chose it, shows a flush failure
/// (which also makes the run exit non-zero), and lists changed memories left
/// alone without `--update`, refused ids and shared ids.
#[test]
fn store_line_names_palace_source_and_every_notice() {
    let counts = StoreCounts {
        changed: 2,
        refused_ids: vec!["m-9".to_string()],
        shared_ids: vec!["m-7".to_string()],
        ..StoreCounts::default()
    };
    let flushed = report(StoreStatus::Imported, counts.clone(), Some("disk full"));
    assert!(
        flushed.is_bad(),
        "a flush failure makes the run exit non-zero"
    );
    assert!(!report(StoreStatus::Imported, counts.clone(), None).is_bad());

    let line = format_report(&flushed, false);
    for want in [
        "-> proj-palace (pin file)",
        "palace flush failed: disk full",
        "2 memory(ies) changed in kuzu since import; re-run with --update",
        "refused 1 secret-shaped memory(ies): m-9",
        "skipped 1 memory id(s) another store holds with other content: m-7",
    ] {
        assert!(line.contains(want), "missing {want:?} in {line}");
    }
    let updating = format_report(&flushed, true);
    assert!(!updating.contains("re-run with --update"), "{updating}");
}
