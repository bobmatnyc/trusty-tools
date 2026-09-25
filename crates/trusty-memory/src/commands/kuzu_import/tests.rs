//! Tests for `import kuzu` (#277).
//!
//! No test needs Python or a Kuzu database: the bridge runs through
//! [`FakeRunner`], which writes a fixture export where `export.py` would.
//! Fixtures follow the shapes in kuzu-memory's source — the Memory columns of
//! `export_memories_to_json` and the edge rows `export.py` emits — and carry
//! only synthetic text.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{json, Value};
use trusty_common::memory_core::palace::{Drawer, Palace, PalaceId};
use trusty_common::memory_core::store::Triple;
use trusty_common::memory_core::{PalaceHandle, PalaceRegistry};
use uuid::Uuid;

use super::apply::{HandleSink, PalaceSink, PalaceView, StoreCounts};
use super::bridge::{export_store, resolve_python, CommandRunner, KuzuExport, RunOutput};
use super::discovery::{discover, resolve_from, DiscoveredStore};
use super::ledger::{Ledger, MemoryPlan};
use super::mapping::{kuzu_content_hash, map_memory, store_id, MappedMemory};
use super::*;

// ── fixtures ────────────────────────────────────────────────────────────

/// One Memory row with every column `export_memories_to_json` selects.
fn memory_row(id: &str, content: &str, hash: &str) -> Value {
    json!({
        "id": id,
        "content": content,
        "content_hash": hash,
        "created_at": "2025-11-03T10:22:11.123456",
        "accessed_at": null,
        "access_count": 0,
        "memory_type": "semantic",
        "knowledge_type": "note",
        "importance": 0.7,
        "confidence": 1.0,
        "source_type": "conversation",
        "source_speaker": "user",
        "project_tag": "demo",
        "agent_id": "default",
        "user_id": "tester",
        "session_id": "sess-1",
        "metadata": "{\"origin\": \"fixture\", \"nested\": {\"x\": 1}}"
    })
}

const COLUMNS: &[&str] = &[
    "id",
    "content",
    "content_hash",
    "created_at",
    "accessed_at",
    "access_count",
    "memory_type",
    "knowledge_type",
    "importance",
    "confidence",
    "source_type",
    "source_speaker",
    "project_tag",
    "agent_id",
    "user_id",
    "session_id",
    "metadata",
];

/// A three-memory store: two entities, three MENTIONS, one RELATES_TO.
fn fixture() -> Value {
    json!({
        "format": "trusty-kuzu-export/1",
        "schema_version": "1.0",
        "exported_at": "2026-09-24T12:00:00",
        "memory_columns": COLUMNS,
        "memory_count": 3,
        "memories": [
            memory_row("m-1", "synthetic note about the widget factory layout", "h1"),
            memory_row("m-2", "synthetic note about gadget assembly order", "h2"),
            memory_row("m-3", "synthetic note linking widgets and gadgets", "h3"),
        ],
        "entities": [
            {"id": "e-widget", "name": "Widget", "entity_type": "concept"},
            {"id": "e-gadget", "name": "Gadget", "entity_type": "concept"},
        ],
        "mentions": [
            {"memory_id": "m-1", "entity_id": "e-widget", "confidence": 0.9},
            {"memory_id": "m-2", "entity_id": "e-gadget", "confidence": 0.9},
            {"memory_id": "m-3", "entity_id": "e-widget", "confidence": 0.8},
        ],
        "relates_to": [
            {"from_id": "m-3", "to_id": "m-1", "relationship_type": "shared_entity", "strength": 0.6},
        ],
    })
}

/// Triples the fixture produces: 4 entity + 3 mentions + 1 relates_to.
const FIXTURE_TRIPLES: usize = 8;

fn export_of(v: &Value) -> KuzuExport {
    serde_json::from_value(v.clone()).expect("fixture parses")
}

/// What the fake child process does.
#[derive(Clone)]
enum Behavior {
    Write(String),
    Exit(i32, &'static str),
    SpawnError,
    ExitZeroNoFile,
}

/// A [`CommandRunner`] that never starts a process.
struct FakeRunner {
    behavior: Behavior,
    out_path: Mutex<Option<PathBuf>>,
}

impl FakeRunner {
    fn new(behavior: Behavior) -> Self {
        Self {
            behavior,
            out_path: Mutex::new(None),
        }
    }
    fn writing(v: &Value) -> Self {
        Self::new(Behavior::Write(v.to_string()))
    }
    fn temp_dir(&self) -> Option<PathBuf> {
        let out = self.out_path.lock().expect("lock").clone()?;
        out.parent().map(Path::to_path_buf)
    }
}

impl CommandRunner for FakeRunner {
    fn run(&self, _program: &Path, args: &[OsString]) -> std::io::Result<RunOutput> {
        let out = PathBuf::from(&args[2]);
        *self.out_path.lock().expect("lock") = Some(out.clone());
        let ok = |success, code, stderr: &str| RunOutput {
            success,
            code: Some(code),
            stderr: stderr.to_string(),
        };
        match &self.behavior {
            Behavior::Write(body) => {
                std::fs::write(&out, body)?;
                Ok(ok(true, 0, ""))
            }
            Behavior::Exit(code, stderr) => Ok(ok(false, *code, stderr)),
            Behavior::SpawnError => Err(std::io::Error::from(std::io::ErrorKind::NotFound)),
            Behavior::ExitZeroNoFile => Ok(ok(true, 0, "")),
        }
    }
}

/// A store directory on disk (`memories.db` is an empty placeholder file).
fn store_on_disk(root: &Path, project: &str) -> DiscoveredStore {
    let dir = root.join(project).join(".kuzu-memory");
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(dir.join("memories.db"), b"KUZU").expect("db");
    resolve_from(&dir).expect("store")
}

/// A real palace in a temp data root, with the mock embedder seeded.
fn palace(data_root: &Path, name: &str) -> Arc<PalaceHandle> {
    trusty_common::memory_core::retrieval::seed_shared_embedder_with_mock();
    PalaceRegistry::new()
        .create_palace(
            data_root,
            Palace {
                id: PalaceId::new(name),
                name: name.to_string(),
                description: None,
                created_at: chrono::Utc::now(),
                data_dir: data_root.join(name),
            },
        )
        .expect("create palace")
}

/// (drawers, active triples, all triple rows including history).
fn palace_state(h: &PalaceHandle) -> (usize, usize, usize) {
    let drawers = h.kg.load_drawers().expect("drawers").len();
    let active = h.kg.count_active_triples().expect("active");
    let all = h.kg.dump_all_triples().expect("dump").len();
    (drawers, active, all)
}

async fn write_run(sink: &dyn PalaceSink, v: &Value, update: bool) -> StoreCounts {
    run_plan(&export_of(v), "store1", Target::Write(sink), update).await
}

// ── discovery ───────────────────────────────────────────────────────────

/// Why: acceptance 1 — every store under the roots, nested ones included; an
/// unreadable directory is reported and skipped; depth is bounded.
#[test]
fn discovery_finds_nested_stores_and_reports_unreadable() {
    let tmp = tempfile::tempdir().expect("tmp");
    let root = tmp.path();
    store_on_disk(root, "alpha");
    store_on_disk(root, "beta/nested/deeper");
    std::fs::create_dir_all(root.join("gamma/.kuzu-memory")).expect("no-db store");
    store_on_disk(root, "a/b/c/d/e/too-deep");
    let locked = root.join("locked");
    std::fs::create_dir_all(locked.join("inner")).expect("locked");
    set_mode(&locked, 0o000);

    let found = discover(&[root.to_path_buf()], 5);
    set_mode(&locked, 0o755);

    let names: Vec<String> = found
        .stores
        .iter()
        .map(|s| {
            s.project_dir()
                .strip_prefix(canon(root))
                .expect("under root")
                .display()
                .to_string()
        })
        .collect();
    assert_eq!(names, vec!["alpha", "beta/nested/deeper"], "{found:?}");
    assert!(found
        .skipped
        .iter()
        .any(|s| s.path.ends_with("gamma/.kuzu-memory")));
    // Running as root can read a 000 directory; the report is only owed when
    // the read actually fails.
    if std::fs::read_dir(&locked).is_ok() && !is_root() {
        return;
    }
    if !is_root() {
        assert!(
            found
                .skipped
                .iter()
                .any(|s| s.path == locked && s.reason.starts_with("unreadable")),
            "{:?}",
            found.skipped
        );
    }
}

/// Why: acceptance 1 — a symlink loop must not hang the walk, and a store
/// reachable only through a symlink is not followed.
#[test]
fn discovery_does_not_follow_symlink_loops() {
    let tmp = tempfile::tempdir().expect("tmp");
    let root = tmp.path();
    store_on_disk(root, "real");
    std::os::unix::fs::symlink(root, root.join("real/loop")).expect("loop");
    std::os::unix::fs::symlink(root.join("real"), root.join("alias")).expect("alias");
    let found = discover(&[root.to_path_buf(), root.to_path_buf()], 50);
    assert_eq!(found.stores.len(), 1, "one store, found once: {found:?}");
}

/// Why: `--from` accepts the `.kuzu-memory` dir or the db inside it, and
/// refuses anything else (the old `store.redb` path included).
#[test]
fn resolve_from_accepts_dir_or_db() {
    let tmp = tempfile::tempdir().expect("tmp");
    let store = store_on_disk(tmp.path(), "p");
    assert_eq!(resolve_from(&store.db).expect("db"), store);
    assert_eq!(resolve_from(&store.dir).expect("dir"), store);
    let redb = tmp.path().join("store.redb");
    std::fs::write(&redb, b"x").expect("redb");
    assert!(matches!(
        resolve_from(&redb),
        Err(KuzuImportError::NotAStore(_))
    ));
}

fn set_mode(p: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).expect("chmod");
}

fn canon(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).expect("canon")
}

fn is_root() -> bool {
    // SAFETY: geteuid has no preconditions.
    unsafe { libc::geteuid() == 0 }
}

// ── bridge ──────────────────────────────────────────────────────────────

/// Why: acceptance 2 — the bridge parses the real export shape and edges.
#[test]
fn bridge_parses_real_export_shape() {
    let runner = FakeRunner::writing(&fixture());
    let export =
        export_store(&runner, Path::new("python"), Path::new("memories.db")).expect("export");
    assert_eq!(export.memories.len(), 3);
    assert_eq!(export.entities.len(), 2);
    assert_eq!(export.edge_count(), 4);
    let m = &export.memories[0];
    assert_eq!(m.id.as_deref(), Some("m-1"));
    assert_eq!(m.importance, Some(0.7));
    assert_eq!(
        export.relates_to[0].relationship_type.as_deref(),
        Some("shared_entity")
    );
}

/// Why: acceptance 5 — the export's temp directory is gone after success and
/// after failure, so a dry run leaves no files behind.
#[test]
fn bridge_leaves_no_temp_output() {
    for behavior in [
        Behavior::Write(fixture().to_string()),
        Behavior::Exit(1, "boom"),
    ] {
        let runner = FakeRunner::new(behavior);
        let _ = export_store(&runner, Path::new("python"), Path::new("memories.db"));
        let dir = runner.temp_dir().expect("runner saw an output path");
        assert!(!dir.exists(), "temp dir {} must be removed", dir.display());
    }
}

/// Why: acceptance 6 — each bridge failure is a typed error, not a panic or
/// a silently empty import.
#[test]
fn bridge_failure_arms_are_typed_errors() {
    let run = |b: Behavior| export_store(&FakeRunner::new(b), Path::new("py"), Path::new("db"));
    assert!(matches!(
        run(Behavior::SpawnError),
        Err(KuzuImportError::InterpreterNotFound(_))
    ));
    assert!(matches!(
        run(Behavior::Exit(
            2,
            "ModuleNotFoundError: No module named 'kuzu'"
        )),
        Err(KuzuImportError::BridgeFailed { code: Some(2), .. })
    ));
    assert!(matches!(
        run(Behavior::ExitZeroNoFile),
        Err(KuzuImportError::MalformedExport(_))
    ));
    assert!(matches!(
        run(Behavior::Write("{not json".to_string())),
        Err(KuzuImportError::MalformedExport(_))
    ));
    let mut old = fixture();
    old["memory_columns"] = json!(["id", "created_at"]);
    assert!(matches!(
        run(Behavior::Write(old.to_string())),
        Err(KuzuImportError::SchemaColumnsMissing(cols)) if cols == vec!["content".to_string()]
    ));
    let mut no_id = fixture();
    no_id["memories"][1]["id"] = Value::Null;
    assert!(matches!(
        run(Behavior::Write(no_id.to_string())),
        Err(KuzuImportError::MalformedExport(_))
    ));
}

/// Why: the interpreter is kuzu-memory's own, found from its launcher.
#[test]
fn resolve_python_reads_the_shebang() {
    let tmp = tempfile::tempdir().expect("tmp");
    let bin = tmp.path();
    let py = bin.join("python3");
    std::fs::write(&py, b"").expect("py");
    let path_var = OsString::from(bin);
    assert!(matches!(
        resolve_python(None, Some(&path_var)),
        Err(KuzuImportError::InterpreterNotFound(_))
    ));
    std::fs::write(
        bin.join("kuzu-memory"),
        format!("#!{}\nimport x\n", py.display()),
    )
    .expect("launcher");
    assert_eq!(resolve_python(None, Some(&path_var)).expect("shebang"), py);
    std::fs::write(bin.join("kuzu-memory"), "#!/usr/bin/env python3\n").expect("env launcher");
    assert_eq!(resolve_python(None, Some(&path_var)).expect("env"), py);
    assert_eq!(resolve_python(Some(&py), None).expect("explicit"), py);
    assert!(resolve_python(Some(&bin.join("nope")), None).is_err());
}

// ── mapping and ledger ──────────────────────────────────────────────────

/// Why: the identity and hash tags are what make a re-run idempotent.
#[test]
fn mapping_tags_identity_hash_and_columns() {
    let export = export_of(&fixture());
    let m = map_memory(&export.memories[0], "s1").expect("mapped");
    assert_eq!(m.source_key, "kuzu-memory/s1/m-1");
    assert!(m.tags.contains(&"source:kuzu-memory/s1/m-1".to_string()));
    assert!(m.tags.contains(&"kuzu-hash:h1".to_string()));
    assert!(m.tags.contains(&"memory_type:semantic".to_string()));
    assert!(m.tags.contains(&"meta:origin:fixture".to_string()));
    assert!(
        !m.tags.iter().any(|t| t.starts_with("agent:")),
        "agent 'default' is noise"
    );
    assert!(
        !m.tags.iter().any(|t| t.starts_with("meta:nested")),
        "non-scalar skipped"
    );
    assert!((m.importance - 0.7).abs() < 1e-6);
    assert_eq!(m.created_at.map(|t| t.timestamp()), Some(1_762_165_331));

    let mut old = export.memories[0].clone();
    old.content_hash = None;
    let m = map_memory(&old, "s1").expect("mapped");
    assert_eq!(
        m.hash,
        kuzu_content_hash("  SYNTHETIC note about the widget factory layout ")
    );
    old.content = Some("   ".to_string());
    assert!(map_memory(&old, "s1").is_none(), "empty content is skipped");
}

/// Why: acceptance 4 — the store id, and so the identity tag, is the same on
/// every run from the same place and differs between stores.
#[test]
fn store_id_is_stable() {
    let tmp = tempfile::tempdir().expect("tmp");
    let a = store_on_disk(tmp.path(), "a");
    let b = store_on_disk(tmp.path(), "b");
    assert_eq!(store_id(&a.dir), store_id(&a.dir));
    assert_eq!(store_id(&a.dir).len(), 12);
    assert_ne!(store_id(&a.dir), store_id(&b.dir));
}

/// Why: acceptance 7 at the planning layer.
#[test]
fn ledger_plans_new_unchanged_changed() {
    let mapped = |hash: &str| MappedMemory {
        source_key: "kuzu-memory/s/m".to_string(),
        memory_id: "m".to_string(),
        content: "c".to_string(),
        hash: hash.to_string(),
        created_at: None,
        importance: 0.5,
        tags: vec![],
    };
    assert_eq!(Ledger::default().plan(&mapped("h")), MemoryPlan::New);
    let mut d = Drawer::new(Uuid::nil(), "c");
    d.tags = vec![
        "source:kuzu-memory/s/m".to_string(),
        "kuzu-hash:h".to_string(),
    ];
    let ledger = Ledger::from_drawers([&d]);
    assert_eq!(ledger.len(), 1);
    assert_eq!(ledger.plan(&mapped("h")), MemoryPlan::Unchanged(d.id));
    assert_eq!(ledger.plan(&mapped("other")), MemoryPlan::Changed(d.id));
}

// ── palace state ────────────────────────────────────────────────────────

/// Why: acceptance 3 — a second import of the same store adds zero drawers
/// and zero triples (active or history), asserted on the palace itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn import_twice_is_idempotent_on_palace_state() {
    let tmp = tempfile::tempdir().expect("tmp");
    let sink = HandleSink {
        handle: palace(tmp.path(), "kuzu-idem"),
    };
    let first = write_run(&sink, &fixture(), false).await;
    assert_eq!(
        (first.new_memories, first.new_triples, first.failed_writes),
        (3, FIXTURE_TRIPLES, 0)
    );
    let after_first = palace_state(&sink.handle);
    assert_eq!(after_first.0, 3);
    assert_eq!(after_first.1, FIXTURE_TRIPLES);

    let second = write_run(&sink, &fixture(), false).await;
    assert_eq!(
        (second.new_memories, second.unchanged, second.new_triples),
        (0, 3, 0)
    );
    assert_eq!(second.existing_triples, FIXTURE_TRIPLES);
    assert_eq!(
        palace_state(&sink.handle),
        after_first,
        "second run must not write"
    );
    assert!(matches!(status_for(&second, false), StoreStatus::UpToDate));
}

/// Why: acceptance 7 — a memory whose hash changed is flagged and left alone
/// without `--update`, then rewritten in the same drawer with it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn changed_hash_is_flagged_then_updated_in_place() {
    let tmp = tempfile::tempdir().expect("tmp");
    let sink = HandleSink {
        handle: palace(tmp.path(), "kuzu-changed"),
    };
    write_run(&sink, &fixture(), false).await;
    let find = |h: &PalaceHandle| -> Drawer {
        h.drawers
            .read()
            .iter()
            .find(|d| {
                d.tags
                    .contains(&"source:kuzu-memory/store1/m-2".to_string())
            })
            .cloned()
            .expect("m-2 drawer")
    };
    let before = find(&sink.handle);

    let mut edited = fixture();
    edited["memories"][1]["content"] = json!("synthetic note about the revised gadget order");
    edited["memories"][1]["content_hash"] = json!("h2-new");
    let flagged = write_run(&sink, &edited, false).await;
    assert_eq!(
        (flagged.changed, flagged.updated, flagged.new_memories),
        (1, 0, 0)
    );
    assert_eq!(
        find(&sink.handle).content(),
        before.content(),
        "no --update: untouched"
    );

    let updated = write_run(&sink, &edited, true).await;
    assert_eq!(
        (updated.changed, updated.updated, updated.failed_writes),
        (1, 1, 0)
    );
    let after = find(&sink.handle);
    assert_eq!(after.id, before.id, "same drawer, not a new one");
    assert!(after.content().contains("revised"));
    assert!(after.tags.contains(&"kuzu-hash:h2-new".to_string()));
    assert!(!after.tags.contains(&"kuzu-hash:h2".to_string()));
    assert_eq!(palace_state(&sink.handle).0, 3);
    let persisted = sink
        .handle
        .kg
        .load_drawer(before.id)
        .expect("load")
        .expect("row");
    assert!(
        persisted.content().contains("revised"),
        "update is persisted, not only cached"
    );
}

/// Why: acceptance 5 — a dry run writes nothing to the palace and leaves no
/// export file behind, yet reports what a real run would do.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dry_run_writes_nothing_and_leaves_no_files() {
    let tmp = tempfile::tempdir().expect("tmp");
    let sink = HandleSink {
        handle: palace(tmp.path(), "kuzu-dry"),
    };
    let runner = FakeRunner::writing(&fixture());
    let store = store_on_disk(tmp.path(), "proj");
    let export = fetch(&runner, Path::new("python"), &store).expect("fetch");
    let counts = run_plan(&export, "store1", Target::DryRun(&sink), false).await;
    assert_eq!(
        (counts.new_memories, counts.new_triples),
        (3, FIXTURE_TRIPLES)
    );
    assert_eq!(
        palace_state(&sink.handle),
        (0, 0, 0),
        "dry run must not write"
    );
    assert!(
        !runner.temp_dir().expect("out").exists(),
        "no export file left"
    );
    assert!(matches!(
        status_for(&counts, true),
        StoreStatus::WouldImport
    ));
}

/// A sink that fails its `fail_on`-th insert, then behaves.
struct FailingSink {
    inner: HandleSink,
    inserts: Mutex<usize>,
    fail_on: usize,
}

#[async_trait]
impl PalaceView for FailingSink {
    fn drawers(&self) -> Vec<Drawer> {
        self.inner.drawers()
    }
    async fn triple_is_active(&self, t: &Triple) -> Result<bool, KuzuImportError> {
        self.inner.triple_is_active(t).await
    }
}

#[async_trait]
impl PalaceSink for FailingSink {
    async fn insert_memory(&self, m: &MappedMemory) -> Result<Uuid, KuzuImportError> {
        let n = {
            let mut g = self.inserts.lock().expect("lock");
            *g += 1;
            *g
        };
        if n == self.fail_on {
            return Err(KuzuImportError::Palace("injected".to_string()));
        }
        self.inner.insert_memory(m).await
    }
    async fn update_memory(&self, id: Uuid, m: &MappedMemory) -> Result<(), KuzuImportError> {
        self.inner.update_memory(id, m).await
    }
    async fn assert_triple(&self, t: Triple) -> Result<(), KuzuImportError> {
        self.inner.assert_triple(t).await
    }
}

/// Why: acceptance 6 (partial write) — a failed write is reported as a
/// partial import, never as done, and the next run completes it without
/// duplicating what landed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn partial_write_failure_is_reported_and_resumable() {
    let tmp = tempfile::tempdir().expect("tmp");
    let handle = palace(tmp.path(), "kuzu-partial");
    let failing = FailingSink {
        inner: HandleSink {
            handle: Arc::clone(&handle),
        },
        inserts: Mutex::new(0),
        fail_on: 1,
    };
    let first = write_run(&failing, &fixture(), false).await;
    // m-1 failed: its drawer, its MENTIONS edge and the RELATES_TO into it.
    assert_eq!((first.new_memories, first.failed_writes), (2, 3));
    assert!(matches!(status_for(&first, false), StoreStatus::Partial));

    let sink = HandleSink { handle };
    let second = write_run(&sink, &fixture(), false).await;
    assert_eq!(
        (second.new_memories, second.unchanged, second.failed_writes),
        (1, 2, 0)
    );
    let (drawers, active, _) = palace_state(&sink.handle);
    assert_eq!((drawers, active), (3, FIXTURE_TRIPLES));
}

/// Why: acceptance 6 — every failure before the first write (bridge arms,
/// empty store) ends the store without creating or touching the palace.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failure_arms_and_empty_store_write_nothing() {
    let tmp = tempfile::tempdir().expect("tmp");
    let data_root = tmp.path().join("data");
    let store = store_on_disk(tmp.path(), "proj");
    let args = KuzuImportArgs {
        palace: Some("kuzu-arms".to_string()),
        ..KuzuImportArgs::default()
    };
    let mut empty = fixture();
    for key in ["memories", "entities", "mentions", "relates_to"] {
        empty[key] = json!([]);
    }
    let mut missing = fixture();
    missing["memory_columns"] = json!(["id"]);
    let cases = [
        Behavior::SpawnError,
        Behavior::Exit(1, "Traceback"),
        Behavior::Write("[]".to_string()),
        Behavior::Write(missing.to_string()),
        Behavior::Write(empty.to_string()),
    ];
    for (i, b) in cases.into_iter().enumerate() {
        let runner = FakeRunner::new(b);
        let report = import_one(&runner, Path::new("py"), &store, &args, &data_root).await;
        let expect_empty = i == 4;
        assert_eq!(
            matches!(report.status, StoreStatus::Empty),
            expect_empty,
            "case {i}: {:?}",
            report.status
        );
        if !expect_empty {
            assert!(matches!(report.status, StoreStatus::Failed(_)), "case {i}");
        }
        assert!(
            !data_root.join("kuzu-arms").exists(),
            "case {i} created the palace"
        );
        assert!(
            !runner.temp_dir().expect("out").exists(),
            "case {i} left a temp dir"
        );
    }
}

/// Why: the dry-run open must not create a palace that does not exist.
#[test]
fn snapshot_view_of_a_missing_palace_is_empty_and_creates_nothing() {
    let tmp = tempfile::tempdir().expect("tmp");
    let view = open_snapshot_view(tmp.path(), "absent").expect("view");
    assert!(view.drawers().is_empty());
    assert!(!tmp.path().join("absent").exists());
}

// ── CLI surface ─────────────────────────────────────────────────────────

/// Why: the flag contract — `--palace` needs `--from`, and `--from` excludes
/// the walk flags.
#[test]
fn import_kuzu_cli_parses_flags() {
    use clap::Parser;
    #[derive(Parser)]
    struct Cli {
        #[command(subcommand)]
        source: ImportSource,
    }
    let parse =
        |args: &[&str]| Cli::try_parse_from(std::iter::once("t").chain(args.iter().copied()));
    let ImportSource::Kuzu(a) = parse(&[
        "kuzu",
        "--discover",
        "--root",
        "/a",
        "--root",
        "/b",
        "--dry-run",
    ])
    .expect("parse")
    .source;
    assert!(a.discover && a.dry_run && !a.update);
    assert_eq!(a.root, vec![PathBuf::from("/a"), PathBuf::from("/b")]);
    assert_eq!(a.max_depth, DEFAULT_MAX_DEPTH);
    assert!(
        parse(&["kuzu", "--palace", "p"]).is_err(),
        "--palace needs --from"
    );
    assert!(parse(&["kuzu", "--from", "x", "--discover"]).is_err());
    assert!(parse(&["kuzu", "--from", "x", "--palace", "p", "--update"]).is_ok());
}

/// Why: the deprecated alias maps onto `import kuzu --from` and surfaces the
/// new path's errors rather than the old redb reader's.
#[test]
fn deprecated_kuzu_data_forwards_to_import() {
    let a = crate::commands::kuzu_migrate::deprecated_args(Path::new("/x/.kuzu-memory"), "p", true);
    assert_eq!(a.from.as_deref(), Some(Path::new("/x/.kuzu-memory")));
    assert_eq!(a.palace.as_deref(), Some("p"));
    assert!(a.dry_run && !a.discover && !a.update);
    let err = crate::commands::kuzu_migrate::handle_kuzu_data_migrate(
        Path::new("/nonexistent/store.redb"),
        "p",
        true,
        Some(3),
    )
    .expect_err("not a store");
    assert!(
        format!("{err:#}").contains("not a kuzu-memory store"),
        "{err:#}"
    );
}
