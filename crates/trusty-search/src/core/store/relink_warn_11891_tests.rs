//! Issue #11891 — the usearch re-link warnings name the index they concern.
//!
//! Why: the #9450 sweep warned "re-linked N stranded vectors ... at f16" with
//! no index id or path, so a production warning could not be traced to an
//! index.
//! What: a store holding more identical vectors than the sweep's search width
//! always leaves keys stranded, so a compaction emits both re-link warnings.
//! Each must carry the store's snapshot path as the `path` field.
//! Test: this module. Run with `cargo test -p trusty-search --lib relink_warn_11891`.

use std::sync::{Arc, Mutex};

use super::types::{CompactMode, VectorStore};
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
/// least 30 keys can never find themselves and the sweep always warns.
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
            .with_max_level(tracing::Level::WARN)
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
    for warning in [
        "vectors still stranded after",
        "stranded vectors after rebuild",
    ] {
        let line = logged
            .lines()
            .find(|l| l.contains(warning))
            .unwrap_or_else(|| panic!("no `{warning}` warning in:\n{logged}"));
        assert!(
            line.contains(&field),
            "the warning must name the index by `{field}`: {line}"
        );
    }
}
