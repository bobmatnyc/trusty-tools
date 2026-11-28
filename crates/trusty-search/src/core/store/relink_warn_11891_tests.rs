//! Issue #11891 — the usearch re-link warnings name the index they concern.
//!
//! Why: the #9450 sweep warned "re-linked N stranded vectors ... at f16" with
//! no index id or path, so a production warning could not be traced to an
//! index.
//! What: a store holding more identical vectors than the sweep's search width
//! makes a compaction log the per-sweep line with duplicate ties, which must
//! carry the store's snapshot path as the `path` field. Since the
//! duplicate-aware sweep, those ties strand nothing, so no strand warning fires.
//! The last two tests guard criterion 1 (#12109) on each strand warning.
//! Test: this module. Run with `cargo test -p trusty-search --lib relink_warn_11891`.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use super::relink_9450_tests::{damaged_index, BLOBS};
use super::types::{CompactMode, VectorStore};
use super::usearch_relink::{finish, relink_stranded, Outcome};
use super::usearch_store::UsearchStore;

/// Collects every formatted log line written through its subscriber.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("capture lock").extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Identical vectors in the store: more than the sweep's 10-hit width, so at
/// least 30 keys can never find themselves and read as duplicate ties.
const DUPLICATES: usize = 40;

/// The rebuild runs on tokio's blocking pool, so the capture is installed on
/// every runtime thread as well as the test thread.
#[test]
fn the_relink_warnings_name_the_index_path() {
    let cap = Capture::default();
    let writer = cap.clone();
    let dispatch = tracing::Dispatch::new(
        tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_max_level(tracing::Level::INFO)
            .with_ansi(false)
            .without_time()
            .finish(),
    );
    let on_start = dispatch.clone();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .on_thread_start(move || std::mem::forget(tracing::dispatcher::set_default(&on_start)))
        .build()
        .expect("runtime");

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("hnsw.usearch");
    tracing::dispatcher::with_default(&dispatch, || {
        runtime.block_on(async {
            let store = UsearchStore::new(8).expect("store init");
            let items: Vec<(String, Vec<f32>)> = (0..DUPLICATES)
                .map(|i| (format!("dup{i}"), vec![1.0; 8]))
                .collect();
            store.upsert_batch(&items).await.expect("upsert");
            store.save(&path).await.expect("save");
            store
                .compact_graph_now(CompactMode::Always)
                .await
                .expect("compact")
                .expect("report");
        });
    });

    let logged = String::from_utf8(cap.0.lock().expect("capture lock").clone()).expect("utf8");
    let field = format!("path={}", path.display());
    let line = logged
        .lines()
        .find(|l| l.contains("re-link sweep at"))
        .unwrap_or_else(|| panic!("no re-link sweep line in:\n{logged}"));
    assert!(
        line.contains(&field),
        "the sweep line must name the index by `{field}`: {line}"
    );
    assert!(
        line.contains("graph_stranded=0") && !line.contains("duplicate_ties=0"),
        "identical vectors must read as duplicate ties, not strands: {line}"
    );
    assert!(
        !logged.contains("vectors still stranded after"),
        "duplicate ties must not leave keys stranded:\n{logged}"
    );
}

/// Run `f` with WARN-level log lines on this thread captured; returns them.
fn capture_warnings(f: impl FnOnce()) -> String {
    let cap = Capture::default();
    let writer = cap.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_max_level(tracing::Level::WARN)
        .with_ansi(false)
        .without_time()
        .finish();
    tracing::subscriber::with_default(subscriber, f);
    let bytes = cap.0.lock().expect("capture lock").clone();
    String::from_utf8(bytes).expect("utf8")
}

/// The line of `logged` holding `warning`; panics naming both when absent.
fn line_with<'a>(logged: &'a str, warning: &str) -> &'a str {
    logged
        .lines()
        .find(|l| l.contains(warning))
        .unwrap_or_else(|| panic!("no `{warning}` warning in:\n{logged}"))
}

/// #11891 criterion 1: the "re-linked" warning names the index. A churned
/// index has real strands, so `relink_stranded` re-links and warns.
#[test]
fn the_relinked_warning_names_the_index_path() {
    let (index, vectors, survivors) = damaged_index(8_000, 9450);
    let read = |i: usize, buf: &mut [f32]| {
        buf.copy_from_slice(&vectors[survivors[i] as usize]);
        Ok(Some(survivors[i]))
    };
    let path = "/idx/relinked/hnsw.usearch";
    let n = survivors.len();
    let logged = capture_warnings(|| {
        relink_stranded(&index, n, BLOBS.dim, 4, "f16", path, &read).expect("relink");
    });
    let line = line_with(&logged, "stranded vectors after rebuild");
    assert!(line.contains(&format!("path={path}")), "{line}");
}

/// #11891 criterion 1: the "still stranded" warning names the index. No
/// seeded build reliably fails to converge, so `finish` gets an outcome whose
/// last sweep still found strands.
#[test]
fn the_still_stranded_warning_names_the_index_path() {
    let path = "/idx/left/hnsw.usearch";
    let out = Outcome {
        passes: vec![5, 4, 3, 3, 2],
        duplicate_ties: 0,
        relinked: 15,
    };
    let logged = capture_warnings(|| {
        finish(&out, "f16", path, Instant::now());
    });
    let line = line_with(&logged, "vectors still stranded after");
    assert!(line.contains(&format!("path={path}")), "{line}");
}
