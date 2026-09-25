//! Python-backed tests for `import kuzu` (#277 H5).
//!
//! Why: `tests.rs` drives the bridge through a fake runner, so nothing there
//! proves that the real `export.py` reads a real Kuzu store, or that its
//! `read_only=True` open leaves the store byte-identical. These tests do,
//! against temp stores built with kuzu-memory's own interpreter.
//! What: every test is `#[ignore]`d and gated on [`PYTHON_ENV`]; unset, it
//! prints why and returns. Each export test builds one store (current schema,
//! an older schema, or a pending WAL), snapshots every file and directory
//! under the project dir (bytes, length, mtime), runs [`fetch`] through the
//! real [`SystemRunner`], and asserts the rows, the edges and their direction,
//! and an identical snapshot afterwards. `live_import_embedding_time_per_memory`
//! is a timing harness, not a pass/fail check. Temp dirs follow `TMPDIR` and
//! are removed on drop. Run:
//! `TRUSTY_KUZU_PYTHON=<python> cargo test -p trusty-memory -- --ignored --nocapture kuzu_import::live_tests`
//! Test: itself.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};
use trusty_common::memory_core::retrieval::{shared_embedder, shared_embedder_initialized};

use super::bridge::{KuzuExport, SystemRunner};
use super::discovery::{resolve_from, DiscoveredStore, STORE_DB_NAME, STORE_DIR_NAME};
use super::tests::{args_for, FakeDaemon};
use super::*;

/// Names an interpreter that can `import kuzu` and `import kuzu_memory`.
const PYTHON_ENV: &str = "TRUSTY_KUZU_PYTHON";
/// Comma-separated store sizes for the timing harness (default `200,1000`).
const BENCH_SIZES_ENV: &str = "TRUSTY_KUZU_BENCH_SIZES";
/// Memory count the timing harness extrapolates to.
const EXTRAPOLATE_TO: usize = 23_016;
/// Memories in each export-test store.
const ROWS: usize = 5;

/// Builds a synthetic store: `python build.py <memories.db> <variant> <n>`.
///
/// `current` takes kuzu-memory's own `get_schema_ddl()`; `old` predates
/// `content_hash`, `MENTIONS.confidence` and the RELATES_TO properties that
/// `export.py` guards; `wal` is `current` with auto-checkpoint off, ended by
/// `os._exit(0)` so `memories.db.wal` is left pending.
const BUILD_SCRIPT: &str = r#"
import os
import sys

import kuzu

OLD_DDL = """
CREATE NODE TABLE Memory (id STRING PRIMARY KEY, content STRING, created_at TIMESTAMP, memory_type STRING, importance FLOAT);
CREATE NODE TABLE Entity (id STRING PRIMARY KEY, name STRING, entity_type STRING);
CREATE REL TABLE MENTIONS (FROM Memory TO Entity);
CREATE REL TABLE RELATES_TO (FROM Memory TO Memory);
"""
PAD = " The importer embeds each memory once on write; this sentence pads the row."


def main(db, variant, n):
    old = variant == "old"
    if old:
        ddl = OLD_DDL
    else:
        from kuzu_memory.storage.schema import get_schema_ddl
        ddl = get_schema_ddl()
    conn = kuzu.Connection(kuzu.Database(db))
    for stmt in ddl.split(";"):
        if stmt.strip():
            conn.execute(stmt)
    if variant == "wal":
        conn.execute("CALL auto_checkpoint=false")
    hash_col = "" if old else ", content_hash: $h"
    for i in range(int(n)):
        conn.execute(
            "CREATE (:Memory {id: $id, content: $c, memory_type: 'semantic', importance: 0.7, "
            f"created_at: timestamp('2025-01-02 03:04:05'){hash_col}}})",
            {"id": f"m{i}", "c": f"synthetic memory {i} about widget {i % 7}." + PAD * 3}
            | ({} if old else {"h": f"hash{i}"}),
        )
    conn.execute("CREATE (:Entity {id: 'e0', name: 'Widget', entity_type: 'thing'})")
    conn.execute("CREATE (:Entity {id: 'e1', name: 'Gadget', entity_type: 'thing'})")
    ment = "" if old else " {confidence: 0.5}"
    rel = "" if old else " {relationship_type: 'follows', strength: 0.25}"
    edge = "MATCH (a:Memory {{id: '{}'}}), (b:{} {{id: '{}'}}) CREATE (a)-[:{}{}]->(b)"
    conn.execute(edge.format("m0", "Entity", "e0", "MENTIONS", ment))
    conn.execute(edge.format("m1", "Entity", "e1", "MENTIONS", ment))
    conn.execute(edge.format("m1", "Memory", "m0", "RELATES_TO", rel))
    if variant == "wal":
        os._exit(0)


main(*sys.argv[1:])
"#;

/// The interpreter from [`PYTHON_ENV`], or `None` after printing why.
fn live_python() -> Option<PathBuf> {
    match std::env::var_os(PYTHON_ENV) {
        Some(p) if !p.is_empty() => Some(PathBuf::from(p)),
        _ => {
            eprintln!(
                "skipping: {PYTHON_ENV} is unset; set it to an interpreter that can \
                 `import kuzu` and `import kuzu_memory`"
            );
            None
        }
    }
}

/// A temp project dir holding `.kuzu-memory/memories.db` of `variant`.
///
/// The build script is written beside the project dir, not inside it, so the
/// snapshot covers only what the store owns.
fn build_store(python: &Path, variant: &str, n: usize) -> (tempfile::TempDir, DiscoveredStore) {
    let root = tempfile::TempDir::with_prefix("trusty-kuzu-live-").expect("temp root");
    let script = root.path().join("build.py");
    std::fs::write(&script, BUILD_SCRIPT).expect("write build script");
    let store_dir = root.path().join("project").join(STORE_DIR_NAME);
    std::fs::create_dir_all(&store_dir).expect("store dir");
    let out = Command::new(python)
        .arg(&script)
        .arg(store_dir.join(STORE_DB_NAME))
        .arg(variant)
        .arg(n.to_string())
        .output()
        .expect("run build script");
    assert!(
        out.status.success(),
        "build {variant} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let store = resolve_from(&store_dir).expect("built store resolves");
    (root, store)
}

/// One path's state: content digest (files only), length and mtime.
#[derive(Debug, PartialEq, Eq)]
struct Entry {
    sha256: Option<String>,
    len: u64,
    mtime: SystemTime,
}

/// Every file and directory under `root`, keyed by relative path.
///
/// Directories are included for their mtime, which moves when an entry is
/// created or removed inside them, so even a lock file made and deleted during
/// the export shows up.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Entry> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        let meta = std::fs::symlink_metadata(&path).expect("stat");
        let sha256 = if meta.is_dir() {
            for child in std::fs::read_dir(&path).expect("read dir") {
                stack.push(child.expect("dir entry").path());
            }
            None
        } else {
            let bytes = std::fs::read(&path).expect("read file");
            Some(format!("{:x}", Sha256::digest(&bytes)))
        };
        let rel = path.strip_prefix(root).expect("under root").to_path_buf();
        out.insert(
            rel,
            Entry {
                sha256,
                len: meta.len(),
                mtime: meta.modified().expect("mtime"),
            },
        );
    }
    out
}

/// One line per file: path, length, digest prefix, mtime in ns.
fn describe(snap: &BTreeMap<PathBuf, Entry>) -> String {
    snap.iter()
        .filter_map(|(p, e)| {
            let sha = e.sha256.as_deref()?;
            let ns = e.mtime.duration_since(UNIX_EPOCH).unwrap_or_default();
            Some(format!(
                "{} {}B sha256={} mtime_ns={}",
                p.display(),
                e.len,
                &sha[..16],
                ns.as_nanos()
            ))
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Export `store` through the real bridge, asserting it is left untouched.
fn export_untouched(python: &Path, store: &DiscoveredStore, case: &str) -> KuzuExport {
    let project = store.project_dir().to_path_buf();
    let before = snapshot(&project);
    let export = fetch(&SystemRunner::default(), python, store).expect("live export");
    let after = snapshot(&project);
    assert_eq!(
        before, after,
        "[{case}] export changed the store's bytes, mtimes or file set"
    );
    println!("live-export[{case}] before: {}", describe(&before));
    println!("live-export[{case}] after:  {}", describe(&after));
    export
}

/// The rows every built store carries, checked field by field.
///
/// `full` is whether the schema has the guarded columns.
fn assert_rows(export: &KuzuExport, full: bool, case: &str) {
    let mut ids: Vec<_> = export
        .memories
        .iter()
        .filter_map(|m| m.id.clone())
        .collect();
    ids.sort();
    let want: Vec<_> = (0..ROWS).map(|i| format!("m{i}")).collect();
    assert_eq!(ids, want, "[{case}] memory ids");
    let m0 = export
        .memories
        .iter()
        .find(|m| m.id.as_deref() == Some("m0"))
        .expect("m0");
    assert!(m0
        .content
        .as_deref()
        .is_some_and(|c| c.starts_with("synthetic memory 0 about widget 0.")));
    assert_eq!(m0.memory_type.as_deref(), Some("semantic"));
    assert!(m0
        .created_at
        .as_deref()
        .is_some_and(|c| c.starts_with("2025-01-02")));
    let has_hash = export.memory_columns.iter().any(|c| c == "content_hash");
    assert_eq!(has_hash, full, "[{case}] content_hash column");
    assert_eq!(m0.content_hash.as_deref(), full.then_some("hash0"));

    let mut entities: Vec<_> = export
        .entities
        .iter()
        .map(|e| (e.id.clone(), e.name.clone()))
        .collect();
    entities.sort();
    let name = |id: &str, n: &str| (Some(id.to_string()), Some(n.to_string()));
    assert_eq!(entities, vec![name("e0", "Widget"), name("e1", "Gadget")]);

    let mut mentions: Vec<_> = export
        .mentions
        .iter()
        .map(|m| (m.memory_id.clone(), m.entity_id.clone(), m.confidence))
        .collect();
    mentions.sort_by(|a, b| a.0.cmp(&b.0));
    let conf = full.then_some(0.5);
    let edge = |a: &str, b: &str| (Some(a.to_string()), Some(b.to_string()), conf);
    assert_eq!(mentions, vec![edge("m0", "e0"), edge("m1", "e1")]);

    // Direction: the store holds m1 -[RELATES_TO]-> m0, never m0 -> m1.
    assert_eq!(export.relates_to.len(), 1, "[{case}] relates_to count");
    let r = &export.relates_to[0];
    assert_eq!(r.from_id.as_deref(), Some("m1"), "[{case}] RELATES_TO from");
    assert_eq!(r.to_id.as_deref(), Some("m0"), "[{case}] RELATES_TO to");
    assert_eq!(r.relationship_type.as_deref(), full.then_some("follows"));
    assert_eq!(r.strength, full.then_some(0.25));
    println!(
        "live-export[{case}] rows: {} memories, {} entities, {} mentions (m0->e0, m1->e1), \
         {} relates_to (m1->m0); columns: {}",
        export.memories.len(),
        export.entities.len(),
        export.mentions.len(),
        export.relates_to.len(),
        export.memory_columns.join(",")
    );
}

/// Why (#277 H5): the current kuzu-memory schema must export in full.
/// Test: itself.
#[test]
#[ignore = "needs TRUSTY_KUZU_PYTHON (kuzu + kuzu_memory)"]
fn live_export_reads_a_current_schema_store_read_only() {
    let Some(py) = live_python() else { return };
    let (_root, store) = build_store(&py, "current", ROWS);
    let export = export_untouched(&py, &store, "a-current");
    assert_rows(&export, true, "a-current");
}

/// Why (#277 H5): an older store lacks `content_hash` and the edge
/// properties; `export.py` must read it without an `ALTER TABLE`.
/// Test: itself.
#[test]
#[ignore = "needs TRUSTY_KUZU_PYTHON (kuzu + kuzu_memory)"]
fn live_export_reads_an_old_schema_store_read_only() {
    let Some(py) = live_python() else { return };
    let (_root, store) = build_store(&py, "old", ROWS);
    let export = export_untouched(&py, &store, "b-old");
    assert_rows(&export, false, "b-old");
}

/// Why (#277 H5): a store whose writer died before a checkpoint keeps its
/// rows in `memories.db.wal`; a read-only open must see them and must not
/// checkpoint or truncate the WAL.
/// Test: itself.
#[test]
#[ignore = "needs TRUSTY_KUZU_PYTHON (kuzu + kuzu_memory)"]
fn live_export_reads_a_pending_wal_without_checkpointing_it() {
    let Some(py) = live_python() else { return };
    let (_root, store) = build_store(&py, "wal", ROWS);
    let wal = store.dir.join(format!("{STORE_DB_NAME}.wal"));
    let wal_len = std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0);
    assert!(
        wal_len > 0,
        "the build left no pending WAL at {}",
        wal.display()
    );
    let export = export_untouched(&py, &store, "c-wal");
    assert_rows(&export, true, "c-wal");
    assert_eq!(std::fs::metadata(&wal).expect("wal").len(), wal_len);
}

/// Store sizes for the timing harness, from [`BENCH_SIZES_ENV`].
fn bench_sizes() -> Vec<usize> {
    std::env::var(BENCH_SIZES_ENV)
        .unwrap_or_else(|_| "200,1000".to_string())
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect()
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// Why (#277): the operator needs to know how long a full import takes, and
/// the write path embeds every memory with the real model.
/// What: a timing harness. It builds an N-memory store, times the bridge,
/// then a real (non-dry-run) `run_import` into a palace under a temp data
/// root, and separately times one-at-a-time embedding of the same contents.
/// It prints ms per memory and an extrapolation to [`EXTRAPOLATE_TO`]. The
/// daemon probe is faked so the timing never depends on the host's daemon.
/// Must run alone, or an earlier test may have seeded the mock embedder.
/// Test: itself.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "timing harness; needs TRUSTY_KUZU_PYTHON and the fastembed model"]
async fn live_import_embedding_time_per_memory() {
    let Some(py) = live_python() else { return };
    if shared_embedder_initialized() {
        eprintln!("skipping: the shared embedder is already set (a mock?); run this test alone");
        return;
    }
    let t = Instant::now();
    let embedder = shared_embedder().await.expect("real embedder");
    println!("live-bench embedder cold init: {:.0} ms", ms(t.elapsed()));
    let runner = SystemRunner::default();
    let daemon = FakeDaemon(None);
    for n in bench_sizes() {
        let (_root, store) = build_store(&py, "current", n);
        let t = Instant::now();
        let export = fetch(&runner, &py, &store).expect("live export");
        let bridge = t.elapsed();
        assert_eq!(export.memories.len(), n);

        let contents: Vec<String> = export
            .memories
            .iter()
            .filter_map(|m| m.content.clone())
            .collect();
        let t = Instant::now();
        for c in &contents {
            embedder
                .embed_batch(std::slice::from_ref(c))
                .await
                .expect("embed");
        }
        let embed_only = t.elapsed();

        let data = tempfile::TempDir::with_prefix("trusty-kuzu-bench-data-").expect("data root");
        let env = ImportEnv {
            runner: &runner,
            daemon: &daemon,
            python: &py,
            data_root: data.path(),
            env_palace: None,
        };
        let args = args_for(&store, "kuzu-bench", false);
        let t = Instant::now();
        let reports = run_import(&args, std::slice::from_ref(&store), &env)
            .await
            .expect("run import");
        let total = t.elapsed();
        let r = &reports[0];
        assert!(matches!(r.status, StoreStatus::Imported), "{:?}", r.status);
        assert_eq!(r.counts.new_memories, n);
        assert!(r.flush_error.is_none(), "{:?}", r.flush_error);

        let write = total.saturating_sub(bridge);
        let per = ms(write) / n as f64;
        let embed_per = ms(embed_only) / n as f64;
        let bridge_per = ms(bridge) / n as f64;
        let est_min = (per + bridge_per) * EXTRAPOLATE_TO as f64 / 60_000.0;
        println!(
            "live-bench N={n}: bridge {:.0} ms, import {:.0} ms (write {:.0} ms), \
             write {per:.2} ms/memory, embed-only {embed_per:.2} ms/memory; \
             {EXTRAPOLATE_TO} memories ~ {est_min:.1} min",
            ms(bridge),
            ms(total),
            ms(write)
        );
    }
}
