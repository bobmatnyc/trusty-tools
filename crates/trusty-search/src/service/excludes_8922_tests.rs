//! #8922: every ingest path honours the walker's excludes and skips sops files.
//!
//! Why: excludes applied only in the directory walker, so `index_file`, the
//! watcher, the dropped-event rescan and boot reconcile could each index a
//! file the walk skips, and nothing checked content for sops metadata.
//! What: one fixture tree — an admitted source file, a file under an excluded
//! glob, and a sops-encrypted YAML file on an admitted path — pushed through
//! each path against a fresh indexer, plus the purge and fail-closed arms.
//! Test: `cargo test -p trusty-search -- excludes_8922`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
use crate::core::sops::sample_sops_yaml;
use crate::core::CodeIndexer;
use crate::service::index_admission::{self, apply_modified, Admission, WatchRoots};
use crate::service::watch_rescan::reconcile_with_policy;
use crate::service::write_admission::admits_pushed;
use crate::service::IndexedFiles;

const KEPT: &str = "src/lib.rs";
const EXCLUDED: &str = "secrets/prod.yaml";
const SOPS: &str = "config/app.yaml";
const PLAIN_YAML: &str = "password: hunter2\nhost: db.internal\n";

/// A canonical temp root holding the three fixture files.
fn tree() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    for (rel, content) in [
        (KEPT, "pub fn kept() {}\n".to_string()),
        (EXCLUDED, PLAIN_YAML.to_string()),
        (SOPS, sample_sops_yaml()),
    ] {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    (temp, root)
}

/// A handle over a fresh indexer; `excluded` adds the `secrets/` glob.
fn handle(id: &str, root: &Path, excluded: bool) -> IndexHandle {
    let indexer = Arc::new(RwLock::new(CodeIndexer::new(id, root)));
    let mut handle = IndexHandle::bare(IndexId::new(id), indexer, root.to_path_buf());
    if excluded {
        handle.exclude_globs = vec!["**/secrets/**".into()];
    }
    handle
}

async fn ids(indexer: &Arc<RwLock<CodeIndexer>>, rel: &str) -> Vec<String> {
    indexer.read().await.chunk_ids_for_file(rel).await
}

/// Assert the fixture's outcome: only `KEPT` holds chunks.
async fn only_kept_is_indexed(path_name: &str, indexer: &Arc<RwLock<CodeIndexer>>) {
    assert!(
        !ids(indexer, KEPT).await.is_empty(),
        "{path_name}: the admitted file must be indexed"
    );
    assert!(
        ids(indexer, EXCLUDED).await.is_empty(),
        "{path_name}: a file under an exclude glob must not be indexed"
    );
    assert!(
        ids(indexer, SOPS).await.is_empty(),
        "{path_name}: a sops-encrypted file must not be indexed"
    );
}

/// #8922: the walker, `index_file`, the watcher, the rescan and boot reconcile
/// each index the admitted file and neither the excluded nor the sops one.
/// Fails with the #8922 fixes disabled on the watcher, rescan and boot arms (the sops
/// file and, for boot, the excluded file are indexed) and on the pushed arm.
#[tokio::test]
async fn every_ingest_path_skips_a_file_the_walker_excludes() {
    let (_temp, root) = tree();
    let roots = WatchRoots {
        canonical: &root,
        raw: &root,
    };
    let rels = [KEPT, EXCLUDED, SOPS];

    // Walker: the excluded file is never walked.
    let walker = handle("x8922-walk", &root, true);
    let walked = index_admission::walk(&walker).files;
    assert!(walked.contains(&root.join(KEPT)));
    assert!(!walked.contains(&root.join(EXCLUDED)), "{walked:?}");

    // index_file: the pushed write is judged by the same decision.
    let pushed = handle("x8922-push", &root, true);
    assert_eq!(admits_pushed(&pushed, KEPT, 20), Admission::Included);
    assert_eq!(admits_pushed(&pushed, EXCLUDED, 20), Admission::Excluded);

    // Watcher.
    let registry = IndexRegistry::new();
    let watched = registry.register(handle("x8922-watch", &root, true));
    let files = IndexedFiles::new();
    for rel in rels {
        apply_modified(
            &registry,
            &watched.id,
            &root.join(rel),
            roots,
            &watched.indexer,
            &files,
            None,
        )
        .await;
    }
    only_kept_is_indexed("watcher", &watched.indexer).await;

    // Dropped-event rescan.
    let rescanned = handle("x8922-rescan", &root, true);
    reconcile_with_policy(
        &rescanned.id,
        &root,
        &root,
        &rescanned.indexer,
        &IndexedFiles::new(),
        Some(&rescanned),
    )
    .await
    .unwrap();
    only_kept_is_indexed("rescan", &rescanned.indexer).await;

    // Boot reconcile's per-file delta.
    let booted = Arc::new(handle("x8922-boot", &root, true));
    let delta: Vec<String> = rels.iter().map(|r| r.to_string()).collect();
    assert!(super::reconcile::apply_delta(&booted, "x8922-boot", &delta, "sha-1").await);
    only_kept_is_indexed("boot reconcile", &booted.indexer).await;
}

/// #8922: boot reconcile purges a file that is already indexed although the
/// policy now excludes it, and a file whose content became sops-encrypted.
/// Fails with the #8922 fixes disabled: the old skip-dir check let `secrets/` through to
/// `index_file`, and nothing removed the sops file's plaintext chunks.
#[tokio::test]
async fn boot_reconcile_delta_honours_the_walker_policy() {
    let (_temp, root) = tree();
    let booted = Arc::new(handle("x8922-purge", &root, true));
    for rel in [EXCLUDED, SOPS] {
        booted
            .indexer
            .read()
            .await
            .index_file(rel, PLAIN_YAML)
            .await
            .unwrap();
        assert!(!ids(&booted.indexer, rel).await.is_empty(), "setup: {rel}");
    }
    let delta = vec![EXCLUDED.to_string(), SOPS.to_string()];
    assert!(super::reconcile::apply_delta(&booted, "x8922-purge", &delta, "sha-2").await);
    assert!(ids(&booted.indexer, EXCLUDED).await.is_empty());
    assert!(ids(&booted.indexer, SOPS).await.is_empty());
}

/// #8922 fail-open check: a delta file the filesystem cannot resolve is
/// neither indexed nor removed, and a delta of only such files does not stamp
/// the SHA. The fixture is a self-referential symlink (`ELOOP`), as in
/// `a_transient_canonicalize_failure_keeps_chunks_and_schedules_a_rescan`.
/// Fails with the #8922 fixes disabled: `exists()` read the loop as "deleted" and the
/// removal arm dropped the file's chunks.
#[cfg(unix)]
#[tokio::test]
async fn boot_reconcile_delta_leaves_an_undetermined_file_alone() {
    let (_temp, root) = tree();
    let booted = Arc::new(handle("x8922-loop", &root, false));
    booted
        .indexer
        .read()
        .await
        .index_file("src/loop.rs", "pub fn looped() {}\n")
        .await
        .unwrap();
    std::os::unix::fs::symlink("loop.rs", root.join("src/loop.rs")).unwrap();
    assert_eq!(
        index_admission::admits(&booted, &root.join("src/loop.rs")),
        Admission::Undetermined,
        "the fixture must produce an undecidable admission"
    );

    let delta = vec!["src/loop.rs".to_string()];
    let stamped = super::reconcile::apply_delta(&booted, "x8922-loop", &delta, "sha-3").await;
    assert!(!stamped, "an undecidable delta must not stamp the SHA");
    assert!(
        !ids(&booted.indexer, "src/loop.rs").await.is_empty(),
        "an undecidable admission must never remove chunks"
    );
}

/// #8922: a rescan drops a tracked file the policy now excludes and a walked
/// file whose content became sops-encrypted, while both are still on disk.
/// Fails with the #8922 fixes disabled: the sweep kept every file still on disk, and
/// the sops file's plaintext chunks survived its re-encryption.
#[tokio::test]
async fn rescan_drops_sops_files_and_files_the_policy_now_excludes() {
    let (_temp, root) = tree();
    std::fs::write(root.join(SOPS), PLAIN_YAML).unwrap();
    let registry = IndexRegistry::new();
    let open = registry.register(handle("x8922-sweep", &root, false));
    let files = IndexedFiles::new();
    let roots = WatchRoots {
        canonical: &root,
        raw: &root,
    };
    for rel in [KEPT, EXCLUDED, SOPS] {
        apply_modified(
            &registry,
            &open.id,
            &root.join(rel),
            roots,
            &open.indexer,
            &files,
            None,
        )
        .await;
        assert!(!ids(&open.indexer, rel).await.is_empty(), "setup: {rel}");
    }

    std::fs::write(root.join(SOPS), sample_sops_yaml()).unwrap();
    let mut narrowed = IndexHandle::bare(open.id.clone(), open.indexer.clone(), root.clone());
    narrowed.exclude_globs = vec!["**/secrets/**".into()];
    let stats = reconcile_with_policy(
        &open.id,
        &root,
        &root,
        &open.indexer,
        &files,
        Some(&narrowed),
    )
    .await
    .unwrap();
    assert_eq!(stats.files_excluded, 2, "{stats:?}");
    assert_eq!(stats.files_removed, 0, "nothing was deleted from disk");
    only_kept_is_indexed("rescan sweep", &open.indexer).await;
}
