//! Regression coverage for #7379: the daemon's watcher applies the index's own
//! file filters, read live from the registry on every event.
//!
//! Why: `index_admission` carries the shared admission policy (#7396), but its
//! own tests call `apply_modified` directly. Nothing pinned the production
//! wiring — `SearchAppState` building its `WatcherManager` with the registry,
//! and the watch loop routing a `Modified` event through admission — so a
//! revert of either one to the registry-less constructor re-admits
//! `registry.toml` and excluded notes while every suite stays green.
//! What: drives the daemon state's own `watcher_manager` over a real OS
//! watcher; pins the fail-closed answer for an event whose index has no
//! registered handle; pins the dropped-event rescan's live registry lookup.
//! Test: this module.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::RwLock;

use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
use crate::core::CodeIndexer;
use crate::service::index_admission::{apply_modified, WatchRoots};
use crate::service::network_fs::MountKind;
use crate::service::watch_rescan::{reconcile_registered, RescanGate};
use crate::service::watch_test_support::{await_watch_condition, await_watch_condition_within};
use crate::service::watcher::WatchEvent;
use crate::service::{IndexedFiles, SearchAppState};

/// Paths the #7379 policy (`include_paths=[notes]`, `extensions=[md]`,
/// `exclude_globs=[**/private.md]`) must keep out of the corpus. `.gitignore`
/// and `registry.toml` are the bookkeeping files the issue saw admitted;
/// `notes/private.md` is inside the subtree and has the right extension, so
/// only the exclude glob keeps it out.
const EXCLUDED: [&str; 3] = [".gitignore", "registry.toml", "notes/private.md"];

/// The one path the policy admits.
const ADMITTED: &str = "notes/x.md";

/// A canonical temp root with the `notes/` subtree the policy selects.
fn fixture_root() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().canonicalize().expect("canonical root");
    std::fs::create_dir(root.join("notes")).expect("mkdir notes");
    (temp, root)
}

/// The #7379 handle: `include_paths=[notes]`, `extensions=[md]`, and an
/// exclude glob for `private.md`.
fn filtered_handle(
    id: &IndexId,
    indexer: &Arc<RwLock<CodeIndexer>>,
    root: &Path,
    extensions: &[&str],
) -> IndexHandle {
    let mut handle = IndexHandle::bare(id.clone(), Arc::clone(indexer), root.to_path_buf());
    handle.include_paths = vec![root.join("notes")];
    handle.extensions = extensions.iter().map(|e| (*e).to_string()).collect();
    handle.exclude_globs = vec!["**/private.md".to_string()];
    handle
}

/// Write every fixture file with content that changes per `generation`, so no
/// content-hash dedupe can swallow a rewrite.
fn write_fixture(root: &Path, generation: u32) {
    let files = [
        (".gitignore", format!("*.log\nbuild-{generation}/\n")),
        (
            "registry.toml",
            format!("[source]\nname = \"synthetic-{generation}\"\nowner = \"atlas\"\n"),
        ),
        (
            "notes/private.md",
            format!("# Private\n\nExcluded synthetic content, revision {generation}.\n"),
        ),
        (
            ADMITTED,
            format!("# Maya\n\nMaya leads the Atlas programme, revision {generation}.\n"),
        ),
    ];
    for (rel, content) in files {
        std::fs::write(root.join(rel), content).expect("write fixture file");
    }
}

async fn has_chunks(indexer: &Arc<RwLock<CodeIndexer>>, rel: &str) -> bool {
    !indexer
        .read()
        .await
        .chunk_ids_for_file(rel)
        .await
        .is_empty()
}

/// #7379: the daemon's own watcher, over real filesystem events, indexes only
/// what the index's filters admit.
///
/// Uses `SearchAppState::new` rather than a hand-built manager, so the state's
/// choice of `WatcherManager::with_registry` is under test too. Fails if
/// either that constructor or the watch loop's `Modified` arm stops routing
/// through `index_admission::apply_modified`: `registry.toml` and
/// `notes/private.md` are then indexed by the unfiltered `handle_modified`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_watcher_honours_include_paths_extensions_and_exclude_globs() {
    let (_temp, root) = fixture_root();
    let id = IndexId::new("watcher-filters-7379-daemon");
    let indexer = Arc::new(RwLock::new(CodeIndexer::new(id.0.clone(), &root)));
    let registry = IndexRegistry::new();
    let handle = registry.register(filtered_handle(&id, &indexer, &root, &["md"]));
    let state = SearchAppState::new(registry);

    state
        .watcher_manager
        .spawn_for_index_with_mount_kind(&handle, MountKind::Local)
        .await;
    assert!(
        state.watcher_manager.is_watching(&id).await,
        "the fixture must actually start a watcher"
    );

    let admitted = {
        let indexer = Arc::clone(&indexer);
        let root = root.clone();
        await_watch_condition(
            |generation| write_fixture(&root, generation),
            move || {
                let indexer = Arc::clone(&indexer);
                async move { has_chunks(&indexer, ADMITTED).await }
            },
        )
        .await
    };
    assert!(
        admitted,
        "the admitted note was never indexed by the watcher"
    );

    // Negative control: keep re-saving the excluded files through a bounded
    // window. `false` is the passing outcome.
    let leaked = {
        let indexer = Arc::clone(&indexer);
        let root = root.clone();
        await_watch_condition_within(
            Duration::from_secs(2),
            |generation| write_fixture(&root, generation),
            move || {
                let indexer = Arc::clone(&indexer);
                async move {
                    for rel in EXCLUDED {
                        if has_chunks(&indexer, rel).await {
                            return true;
                        }
                    }
                    false
                }
            },
        )
        .await
    };
    for rel in EXCLUDED {
        assert!(
            !has_chunks(&indexer, rel).await,
            "the watcher indexed `{rel}`, which the index's filters exclude"
        );
    }
    assert!(!leaked, "an excluded file reached the corpus");

    state.watcher_manager.stop_for_index(&id).await;
}

/// #7379 fail-open check: an event whose index has no registered handle — the
/// filter config cannot be read — indexes nothing, and asks for a rescan.
///
/// The chosen behaviour is skip-and-warn: `apply_modified` logs a warning and
/// defers to the rescan gate, and the rescan itself fails as
/// `UnregisteredIndex` until a handle returns. Falling back to the unfiltered
/// `handle_modified` would index the note here and fail the first assertion.
#[tokio::test(start_paused = true)]
async fn an_event_without_a_readable_policy_indexes_nothing_and_requests_a_rescan() {
    let (_temp, root) = fixture_root();
    write_fixture(&root, 0);
    let id = IndexId::new("watcher-filters-7379-unregistered");
    let indexer = Arc::new(RwLock::new(CodeIndexer::new(id.0.clone(), &root)));
    let registry = IndexRegistry::new();
    let files = IndexedFiles::new();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<WatchEvent>();
    let gate = RescanGate::new(tx);

    apply_modified(
        &registry,
        &id,
        &root.join(ADMITTED),
        WatchRoots {
            canonical: &root,
            raw: &root,
        },
        &indexer,
        &files,
        Some(&gate),
    )
    .await;

    assert!(
        !has_chunks(&indexer, ADMITTED).await,
        "an event with no readable policy must not be indexed unfiltered"
    );
    assert_eq!(files.len().await, 0, "nothing may be recorded as indexed");
    // Sleeping past the base backoff on a paused clock lets the armed timer send.
    tokio::time::sleep(Duration::from_secs(60)).await;
    assert_eq!(
        rx.try_recv().ok(),
        Some(WatchEvent::Rescan),
        "the skipped event must be settled by a rescan, not dropped"
    );
}

/// #7379: the dropped-event rescan reads the filters from the registry on each
/// pass, so it applies the same policy as the event path — including a policy
/// replaced after the watcher started.
#[tokio::test]
async fn dropped_event_rescan_applies_the_live_registry_policy() {
    let (_temp, root) = fixture_root();
    write_fixture(&root, 0);
    let id = IndexId::new("watcher-filters-7379-rescan");
    let indexer = Arc::new(RwLock::new(CodeIndexer::new(id.0.clone(), &root)));
    let registry = IndexRegistry::new();
    registry.register(filtered_handle(&id, &indexer, &root, &["md"]));
    let files = IndexedFiles::new();

    reconcile_registered(&id, &root, &root, &indexer, &files, Some(&registry))
        .await
        .expect("first rescan pass");
    assert!(
        has_chunks(&indexer, ADMITTED).await,
        "the rescan must index the admitted note"
    );
    for rel in EXCLUDED {
        assert!(
            !has_chunks(&indexer, rel).await,
            "the rescan indexed `{rel}`, which the index's filters exclude"
        );
    }

    // Replace the policy in the registry without touching the loop: the next
    // pass must read the new one. `notes/` holds no `.toml`, so widen the
    // subtree to the root for this half.
    let mut widened = filtered_handle(&id, &indexer, &root, &["toml"]);
    widened.include_paths = Vec::new();
    registry.register(widened);
    reconcile_registered(&id, &root, &root, &indexer, &files, Some(&registry))
        .await
        .expect("second rescan pass");
    assert!(
        has_chunks(&indexer, "registry.toml").await,
        "the second pass must apply the replaced policy, not a snapshot"
    );
    assert!(
        !has_chunks(&indexer, ".gitignore").await,
        "`.gitignore` has no `toml` extension and must stay out"
    );
}
