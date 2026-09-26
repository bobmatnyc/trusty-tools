//! #8232: replacing an index's files after `DELETE /indexes/{id}` must not
//! crash the daemon through a handle that outlived the delete.
//!
//! Why: the reported SIGBUS came from a deleted index whose `hnsw.usearch` was
//! still memory-mapped by a surviving `Arc<IndexHandle>` (a deferred-embed job,
//! an in-flight request). Truncating or replacing that file under the mapping
//! turns the next vector search into a bus error, which kills the whole
//! process — every index, not just the deleted one. A SIGBUS cannot be caught
//! by a test harness, so this lives in its own test binary: on pre-fix code
//! the binary dies by signal instead of reporting a failed assertion.
//! What: builds a real colocated index with a saved HNSW snapshot, reloads it
//! so the snapshot is served from the mmap view, keeps a second handle alive,
//! deletes the index through the real router, truncates `hnsw.usearch` to zero
//! bytes, and searches through the surviving handle. The search must return an
//! error that names the deletion. That refusal comes from the indexer's
//! `deleted` flag and never reaches the store, so the test also proves the
//! release itself: the store reports itself closed, no descriptor of this
//! process names `hnsw.usearch` (Linux and macOS), and on Linux
//! `/proc/self/maps` lists no mapping of it.
//! Test: `cargo test -p trusty-search --test delete_releases_mmap_8232`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tokio::sync::RwLock;
use tower::ServiceExt;

use trusty_common::embedder::MockEmbedder;
use trusty_search::core::indexer::SearchQuery;
use trusty_search::core::registry::{IndexHandle, IndexId, IndexRegistry};
use trusty_search::core::Embedder;
use trusty_search::service::persistence::{hnsw_path_for_entry, PersistedIndex};
use trusty_search::service::persistence_loader::build_indexer_from_entry;
use trusty_search::service::server::{build_router, SearchAppState};

const INDEX_ID: &str = "delete-mmap-8232";

/// Colocated entry rooted at `root`, the layout the #8232 host used.
fn colocated_entry(root: PathBuf) -> PersistedIndex {
    let mut entry = PersistedIndex::new(INDEX_ID.to_string(), root);
    entry.colocated = true;
    entry
}

/// Index one file and save the HNSW snapshot, so a later reload maps it.
async fn seed_snapshot(entry: &PersistedIndex, embedder: &Arc<dyn Embedder>) {
    let indexer = build_indexer_from_entry(entry, embedder)
        .await
        .expect("build seed indexer");
    for n in 0..64 {
        let body = format!("pub fn handler_{n}() -> u32 {{ {n} }}\n");
        indexer
            .index_file(&format!("src/file_{n}.rs"), &body)
            .await
            .expect("index_file");
    }
    let hnsw = hnsw_path_for_entry(entry).expect("hnsw path");
    let saved = indexer.save_vector_store(&hnsw).await.expect("save hnsw");
    assert!(saved, "the seed pass must write {}", hnsw.display());
}

/// This process's open descriptors on `target`, or `None` where the platform
/// offers no way to list them.
///
/// Why: usearch keeps the descriptor for as long as the view is mapped, so an
/// open descriptor on `hnsw.usearch` means the mapping is still live.
#[cfg(target_os = "linux")]
fn descriptors_on(target: &Path) -> Option<Vec<PathBuf>> {
    let target = std::fs::canonicalize(target).expect("canonical snapshot path");
    let fds = std::fs::read_dir("/proc/self/fd").expect("list /proc/self/fd");
    Some(
        fds.filter_map(|fd| std::fs::read_link(fd.ok()?.path()).ok())
            .filter(|path| *path == target)
            .collect(),
    )
}

/// macOS form of [`descriptors_on`]: `fcntl(F_GETPATH)` over every descriptor.
#[cfg(target_os = "macos")]
fn descriptors_on(target: &Path) -> Option<Vec<PathBuf>> {
    let target = std::fs::canonicalize(target).expect("canonical snapshot path");
    // SAFETY: `getdtablesize` reads a process limit and has no preconditions.
    let limit = unsafe { libc::getdtablesize() }.clamp(0, 1 << 16);
    let mut found = Vec::new();
    for fd in 0..limit {
        let mut buf = vec![0u8; libc::PATH_MAX as usize];
        // SAFETY: F_GETPATH writes at most PATH_MAX bytes into `buf`; a
        // descriptor that is not open fails with EBADF and writes nothing.
        if unsafe { libc::fcntl(fd, libc::F_GETPATH, buf.as_mut_ptr()) } != 0 {
            continue;
        }
        let Ok(path) = std::ffi::CStr::from_bytes_until_nul(&buf) else {
            continue;
        };
        let path = PathBuf::from(path.to_string_lossy().into_owned());
        if path == target {
            found.push(path);
        }
    }
    Some(found)
}

/// No descriptor listing on this platform; the store-state check still runs.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn descriptors_on(_target: &Path) -> Option<Vec<PathBuf>> {
    None
}

/// Lines of `/proc/self/maps` that map `target` (Linux only).
#[cfg(target_os = "linux")]
fn mappings_of(target: &Path) -> Vec<String> {
    let target = std::fs::canonicalize(target).expect("canonical snapshot path");
    let target = target.to_string_lossy().into_owned();
    std::fs::read_to_string("/proc/self/maps")
        .expect("read /proc/self/maps")
        .lines()
        .filter(|line| line.ends_with(target.as_str()))
        .map(str::to_string)
        .collect()
}

/// #8232: a search through a handle that outlived `DELETE` must fail cleanly
/// after the snapshot file is truncated — never SIGBUS the process.
#[tokio::test]
async fn a_search_through_a_surviving_handle_after_delete_and_truncate_errors() {
    let data_dir = tempfile::tempdir().expect("data dir");
    // SAFETY: the only test in this binary, so no other thread reads the env.
    unsafe { std::env::set_var("TRUSTY_DATA_DIR", data_dir.path()) };
    let root = tempfile::tempdir().expect("root");
    let entry = colocated_entry(root.path().to_path_buf());
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(8));
    seed_snapshot(&entry, &embedder).await;

    // Reload: `load_from` serves the snapshot from an mmap view by default.
    let indexer = build_indexer_from_entry(&entry, &embedder)
        .await
        .expect("reload indexer");
    let registry = IndexRegistry::new();
    let surviving = registry.register(IndexHandle::bare(
        IndexId::new(INDEX_ID),
        Arc::new(RwLock::new(indexer)),
        entry.root_path.clone(),
    ));
    let router = build_router(SearchAppState::new(registry));

    // Preconditions: each release check below can see the live view.
    let hnsw = hnsw_path_for_entry(&entry).expect("hnsw path");
    if let Some(open) = descriptors_on(&hnsw) {
        assert!(
            !open.is_empty(),
            "precondition: the view holds the snapshot open"
        );
    }
    #[cfg(target_os = "linux")]
    assert!(
        !mappings_of(&hnsw).is_empty(),
        "precondition: the view maps the snapshot"
    );

    let resp = router
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/indexes/{INDEX_ID}"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::OK, "the delete must succeed");

    // The release itself, observed below the indexer's `deleted` refusal.
    assert_eq!(
        surviving.indexer.read().await.vector_count().await,
        None,
        "the surviving handle's vector store must be closed, not still serving"
    );
    if let Some(open) = descriptors_on(&hnsw) {
        assert!(
            open.is_empty(),
            "DELETE must close the snapshot's descriptor, and with it the mapping: {open:?}"
        );
    }
    #[cfg(target_os = "linux")]
    {
        let mapped = mappings_of(&hnsw);
        assert!(
            mapped.is_empty(),
            "DELETE must unmap hnsw.usearch: {mapped:?}"
        );
    }

    // The replace-under-live-daemon step from the #8232 delivery recipe.
    std::fs::OpenOptions::new()
        .write(true)
        .open(&hnsw)
        .expect("open snapshot")
        .set_len(0)
        .expect("truncate snapshot");

    let query = SearchQuery {
        text: "handler returns a number".to_string(),
        ..Default::default()
    };
    let outcome = surviving.indexer.read().await.search(&query).await;
    let err = match outcome {
        Ok(hits) => panic!(
            "a deleted index must refuse the search, not answer {} hit(s) — an empty \
             or stale answer reads as a real one",
            hits.len()
        ),
        Err(e) => e,
    };
    assert!(
        format!("{err:#}").contains("deleted"),
        "the refusal must say the index was deleted: {err:#}"
    );
    unsafe { std::env::remove_var("TRUSTY_DATA_DIR") };
}
