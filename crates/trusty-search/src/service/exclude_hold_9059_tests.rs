//! #9059: an index restored with an exclude glob that does not parse is held.
//!
//! Why: #8922 rejects such a glob where it enters, but one restored from
//! `indexes.toml` was skipped at match time, so every ingest path admitted the
//! files it was written to exclude — typically secrets.
//! What: one restored fixture per ingest path, each holding an admitted file
//! that is already indexed and a secrets file the invalid glob meant to
//! exclude. Each path must leave the secrets file unindexed, keep the existing
//! chunks, and (where it answers) name the glob. A second test pins the status
//! surface, that reads keep working, and that a valid PATCH releases the hold.
//! Test: `cargo test -p trusty-search -- exclude_hold_9059`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::core::embed::{Embedder, MockEmbedder};
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
use crate::service::index_admission::{apply_modified, WatchRoots};
use crate::service::persistence::PersistedIndex;
use crate::service::server::{PatchIndexConfigRequest, SearchAppState};
use crate::service::watch_rescan::reconcile_with_policy;
use crate::service::IndexedFiles;

const KEPT: &str = "src/lib.rs";
const SECRET: &str = "secrets/prod.yaml";
const PLAIN_YAML: &str = "password: hunter2\nhost: db.internal\n";
/// A secrets glob with a typo: the unclosed `[` does not parse.
const BAD: &str = "**/secrets/[**";
const FIXED: &str = "**/secrets/**";

/// A canonical temp root holding the admitted file and the secrets file.
fn tree() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    for (rel, content) in [(KEPT, "pub fn kept() {}\n"), (SECRET, PLAIN_YAML)] {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    (temp, root)
}

/// Restore `id` over `root` with [`BAD`] persisted, with [`KEPT`] indexed.
async fn restored(id: &str, root: &Path) -> (Arc<SearchAppState>, Arc<IndexHandle>, IndexedFiles) {
    assert_eq!(
        crate::core::repo_config::invalid_exclude_globs(&[BAD.to_string()]),
        vec![BAD.to_string()],
        "the fixture glob must not parse"
    );
    let state = Arc::new(SearchAppState::new(IndexRegistry::new()));
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(8));
    state.install_embedder(embedder.clone()).await;
    let entry = PersistedIndex {
        id: id.to_string(),
        root_path: root.to_path_buf(),
        colocated: true,
        exclude_globs: vec![BAD.to_string()],
        ..Default::default()
    };
    crate::service::lazy_restore::restore_index_on_demand(&state, &embedder, entry).await;
    let handle = state.registry.get(&IndexId::new(id)).expect("restored");
    let idx = handle.indexer.read().await;
    idx.index_file(KEPT, "pub fn kept() {}\n").await.unwrap();
    drop(idx);
    let files = IndexedFiles::new();
    files
        .record(PathBuf::from(KEPT), ids(&handle, KEPT).await)
        .await;
    assert!(!ids(&handle, KEPT).await.is_empty(), "setup: {id}");
    (state, handle, files)
}

async fn ids(handle: &IndexHandle, rel: &str) -> Vec<String> {
    handle.indexer.read().await.chunk_ids_for_file(rel).await
}

/// `Ok` when `text` names the invalid glob, else why not.
fn names_glob(arm: &str, text: &str) -> Result<(), String> {
    if text.contains(BAD) {
        Ok(())
    } else {
        Err(format!(
            "{arm}: the refusal must name the invalid glob, got: {text}"
        ))
    }
}

/// Drive one ingest path against a held index; `Err` says how it failed open.
async fn run_arm(arm: &str) -> Result<(), String> {
    let (_temp, root) = tree();
    let id = format!("x9059-{arm}");
    let (state, handle, files) = restored(&id, &root).await;
    let secret = root.join(SECRET);
    match arm {
        "watcher" => {
            let roots = WatchRoots {
                canonical: &root,
                raw: &root,
            };
            apply_modified(
                &state.registry,
                &handle.id,
                &secret,
                roots,
                &handle.indexer,
                &files,
                None,
            )
            .await;
        }
        "push" => {
            let req = crate::service::server::IndexFileRequest {
                path: SECRET.to_string(),
                content: PLAIN_YAML.to_string(),
            };
            match crate::service::server::index_file_report(&state, &id, req).await {
                Ok(body) => return Err(format!("push: the write was accepted: {body}")),
                Err((status, body)) => {
                    if status != axum::http::StatusCode::CONFLICT {
                        return Err(format!("push: expected 409, got {status}: {body}"));
                    }
                    names_glob(arm, &body.to_string())?;
                }
            }
        }
        "rescan" => {
            let outcome = reconcile_with_policy(
                &handle.id,
                &root,
                &root,
                &handle.indexer,
                &files,
                Some(&handle),
            )
            .await;
            match outcome {
                Ok(stats) => return Err(format!("rescan: the pass ran: {stats:?}")),
                Err(err) => names_glob(arm, &err.to_string())?,
            }
        }
        "boot" => {
            let delta = vec![SECRET.to_string()];
            if super::reconcile::apply_delta(&handle, &id, &delta, "sha-9059").await {
                return Err("boot: the delta was applied and the SHA stamped".into());
            }
        }
        "reindex-http" => match crate::service::server::reindex_report(&state, &id, None).await {
            Ok(body) => return Err(format!("reindex-http: the reindex was queued: {body}")),
            Err((status, body)) => {
                if status != axum::http::StatusCode::CONFLICT {
                    return Err(format!("reindex-http: expected 409, got {status}: {body}"));
                }
                names_glob(arm, &body.to_string())?;
            }
        },
        _ => {
            let progress = Arc::new(crate::service::reindex::ReindexProgress::new());
            match crate::service::reindex::spawn_reindex(handle.clone(), progress, false) {
                Ok(()) => return Err(format!("{arm}: the reindex was spawned")),
                Err(err) => names_glob(arm, &err.to_string())?,
            }
        }
    }
    if !ids(&handle, SECRET).await.is_empty() {
        return Err(format!("{arm}: the secrets file was indexed"));
    }
    if ids(&handle, KEPT).await.is_empty() {
        return Err(format!("{arm}: the existing chunks were dropped"));
    }
    Ok(())
}

/// #9059 fail-open check: every ingest path — watcher save, pushed
/// `index_file`, dropped-event rescan, boot reconcile's delta, the HTTP
/// reindex and the internal reindex spawn — indexes nothing on a held index,
/// keeps its existing chunks, and names the invalid glob where it answers.
/// Fails against 953709df78, where the glob was skipped: every arm indexed or
/// queued the secrets file.
#[tokio::test]
async fn every_ingest_path_refuses_a_held_index() {
    let mut failures = Vec::new();
    for arm in [
        "watcher",
        "push",
        "rescan",
        "boot",
        "reindex-http",
        "reindex-spawn",
    ] {
        if let Err(why) = run_arm(arm).await {
            failures.push(why);
        }
    }
    assert!(
        failures.is_empty(),
        "held index failed open:\n{}",
        failures.join("\n")
    );
}

/// #9059: a held index reports `status: "held"` with the glob in
/// `last_walk_error`, keeps answering searches, and one valid PATCH releases
/// it without a restart: the excluded path is then refused as excluded, and an
/// admitted one is indexed. Fails against 953709df78, whose status read `ready`.
#[tokio::test]
async fn a_held_index_reports_held_serves_reads_and_a_valid_patch_releases_it() {
    let (_temp, root) = tree();
    let id = "x9059-release";
    let (state, _handle, files) = restored(id, &root).await;

    let status = crate::service::server::index_status_report(&state, id)
        .await
        .expect("status");
    assert_eq!(status["status"], "held", "{status}");
    names_glob("status", &status["last_walk_error"].to_string()).unwrap();
    let view = crate::service::server::index_config_report(&state, id).expect("config");
    assert_eq!(view.invalid_exclude_globs, vec![BAD.to_string()]);

    let query: crate::core::indexer::SearchQuery =
        serde_json::from_value(serde_json::json!({ "text": "kept" })).unwrap();
    let found = crate::service::server::search_report(&state, id, query)
        .await
        .expect("a held index keeps serving reads");
    assert!(found.to_string().contains(KEPT), "{found}");

    let patch = PatchIndexConfigRequest {
        exclude_globs: Some(vec![FIXED.to_string()]),
        ..Default::default()
    };
    crate::service::server::patch_index_config_report(&state, id, patch)
        .await
        .expect("a valid PATCH is accepted");

    let status = crate::service::server::index_status_report(&state, id)
        .await
        .expect("status");
    assert_ne!(status["status"], "held", "{status}");
    let released = state.registry.get(&IndexId::new(id)).expect("registered");
    let refused = crate::service::write_admission::gate(
        &released,
        &*released.indexer.read().await,
        SECRET,
        PLAIN_YAML,
    )
    .await
    .expect_err("the fixed glob excludes the secrets file");
    assert_eq!(
        refused.0,
        axum::http::StatusCode::FORBIDDEN,
        "{}",
        refused.1
    );

    std::fs::write(root.join("src/new.rs"), "pub fn added() {}\n").unwrap();
    let stats = reconcile_with_policy(
        &released.id,
        &root,
        &root,
        &released.indexer,
        &files,
        Some(&released),
    )
    .await
    .expect("a released index rescans");
    assert!(!ids(&released, "src/new.rs").await.is_empty(), "{stats:?}");
    assert!(ids(&released, SECRET).await.is_empty(), "{stats:?}");
}
