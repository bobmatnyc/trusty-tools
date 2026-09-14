//! Coverage for the auto-memory → palace migration (#7685).
//!
//! Why: this command is the reason zeroing `MEMORY.md` is not data loss, so the
//! two properties that make that true have to be pinned: a fact reaches the
//! palace BEFORE its file moves, and a file that did not reach the palace stays
//! exactly where it was — index included.
//! What: drives [`super::run_auto_memory_import`] against the shared stub
//! JSON-RPC daemon over a fixture auto-memory directory.
//! Test: this file.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use crate::uds_mock::RpcError;

use super::{AutoImportOptions, AutoImportStatus, auto_memory_dir, run_auto_memory_import};

/// A representative Claude Code auto-memory fact file.
const FACT: &str = r"---
name: link-issues-and-prs
description: Every #N in a reply is a clickable GitHub link
metadata:
  node_type: memory
  type: feedback
---

The owner asked for this on 2026-09-11.
";

/// A second fact, so ordering and per-file outcomes are both observable.
const OTHER_FACT: &str = r"---
name: arm-only-when-repo-gates-green
description: Disable auto-merge before fixing a red gate
metadata:
  node_type: memory
  type: project
---

Non-required red merged #7194 and reddened main.
";

const INDEX: &str = "- [link issues and prs](link-issues-and-prs.md) — owner ruling\n";

/// Every `memory_remember` the stub saw, plus the methods it refuses.
#[derive(Default)]
struct StubState {
    writes: Vec<Value>,
    deny: HashSet<String>,
    /// #7752: drawer ids the palace has since forgotten — stored, then deleted,
    /// so `memory_list` stops reporting them while the write history remains.
    forgotten: HashSet<String>,
}

type Stub = Arc<Mutex<StubState>>;

fn rpc(state: &Stub, method: &str, params: Value) -> Result<Value, RpcError> {
    let mut st = state.lock().expect("stub lock");
    if st.deny.contains(method) {
        return Err(RpcError::internal(format!("{method} refused by the stub")));
    }
    if method == "memory_remember" {
        st.writes.push(params);
        let id = format!("drawer-{}", st.writes.len());
        return Ok(json!({ "drawer_id": id, "status": "stored" }));
    }
    if method == "memory_list" {
        // #7685: the daemon's exact-tag filter, over what this stub stored.
        let tag = params["tag"].as_str().unwrap_or_default().to_string();
        let drawers: Vec<Value> = st
            .writes
            .iter()
            .enumerate()
            .filter(|(i, w)| {
                !st.forgotten.contains(&format!("drawer-{}", i + 1))
                    && w["tags"]
                        .as_array()
                        .is_some_and(|tags| tags.iter().any(|t| t.as_str() == Some(tag.as_str())))
            })
            .map(|(i, w)| json!({ "drawer_id": format!("drawer-{}", i + 1), "tags": w["tags"] }))
            .collect();
        return Ok(json!({ "palace": params["palace"], "drawers": drawers }));
    }
    Ok(json!({ "status": "ok" }))
}

async fn start_stub() -> (crate::uds_mock::MockUdsDaemon, Stub) {
    let state: Stub = Arc::new(Mutex::new(StubState::default()));
    let served = Arc::clone(&state);
    let daemon = crate::uds_mock::spawn(move |method: &str, params: Value| {
        let state = Arc::clone(&served);
        let method = method.to_string();
        Box::pin(async move { rpc(&state, &method, params) })
    })
    .await;
    (daemon, state)
}

/// Build a fixture `<config>/projects/<slug>/memory/` for `project`.
///
/// Returns the config dir's tempdir and the memory directory inside it.
fn write_store(
    project: &Path,
    facts: &[(&str, &str)],
    index: &str,
) -> (tempfile::TempDir, PathBuf) {
    let config = tempfile::tempdir().expect("tempdir");
    let memory = auto_memory_dir(config.path(), project);
    std::fs::create_dir_all(&memory).expect("create memory dir");
    for (name, body) in facts {
        std::fs::write(memory.join(name), body).expect("write fact");
    }
    std::fs::write(memory.join(super::INDEX_FILE), index).expect("write index");
    (config, memory)
}

fn opts(project: &Path, config: &Path, socket: &Path) -> AutoImportOptions {
    AutoImportOptions {
        project_dir: project.to_path_buf(),
        config_dir: config.to_path_buf(),
        palace: "stub".to_string(),
        memory_socket: Some(socket.to_path_buf()),
        archive_stamp: "20260912".to_string(),
    }
}

#[test]
fn auto_memory_dir_uses_the_claude_project_encoding() {
    let dir = auto_memory_dir(Path::new("/cfg"), Path::new("/Users/x/repo"));
    let encoded = crate::runtime::encode_project_dir(Path::new("/Users/x/repo"));
    assert_eq!(
        dir,
        Path::new("/cfg")
            .join("projects")
            .join(encoded)
            .join("memory")
    );
}

#[test]
fn resolve_auto_import_options_keeps_an_explicit_palace() {
    let project = tempfile::tempdir().expect("tempdir");
    let config = tempfile::tempdir().expect("tempdir");

    let opts = super::resolve_in(
        project.path(),
        config.path().to_path_buf(),
        Some("chosen".to_string()),
        None,
    )
    .expect("an explicit palace needs no derivation");

    assert_eq!(opts.palace, "chosen");
    assert_eq!(
        opts.archive_stamp.len(),
        8,
        "the archive stamp is YYYYMMDD: {}",
        opts.archive_stamp
    );
    assert!(opts.archive_stamp.chars().all(|c| c.is_ascii_digit()));
}

#[test]
fn index_has_content_reads_the_index_file() {
    let tmp = tempfile::tempdir().expect("tempdir");
    assert!(
        !super::index_has_content(tmp.path()),
        "an absent index holds nothing"
    );
    std::fs::write(tmp.path().join(super::INDEX_FILE), "   \n").expect("write");
    assert!(
        !super::index_has_content(tmp.path()),
        "a whitespace-only index is empty"
    );
    std::fs::write(tmp.path().join(super::INDEX_FILE), INDEX).expect("write");
    assert!(super::index_has_content(tmp.path()));
}

#[test]
fn migration_tags_omit_an_empty_type() {
    assert_eq!(
        super::migration_tags("a-slug", "feedback"),
        vec![
            super::MIGRATION_TAG.to_string(),
            "name:a-slug".to_string(),
            "type:feedback".to_string(),
        ]
    );
    assert_eq!(
        super::migration_tags("a-slug", "  "),
        vec![super::MIGRATION_TAG.to_string(), "name:a-slug".to_string()]
    );
}

#[tokio::test]
async fn auto_memory_import_stores_and_archives_each_fact() {
    let project = tempfile::tempdir().expect("tempdir");
    let (config, memory) = write_store(
        project.path(),
        &[
            ("link-issues-and-prs.md", FACT),
            ("arm-only-when-repo-gates-green.md", OTHER_FACT),
        ],
        INDEX,
    );
    let (daemon, state) = start_stub().await;

    let report = run_auto_memory_import(&opts(project.path(), config.path(), daemon.socket()))
        .await
        .expect("migration runs");

    assert_eq!(report.total, 2, "{:#?}", report.files);
    assert_eq!(report.stored, 2, "{:#?}", report.files);
    assert_eq!(report.failed, 0);
    assert!(report.index_cleared);

    // Every drawer carries the three tags the owner ruling names.
    let writes = state.lock().expect("lock").writes.clone();
    assert_eq!(writes.len(), 2);
    let tags: Vec<String> = writes[0]["tags"]
        .as_array()
        .expect("tags")
        .iter()
        .filter_map(|t| t.as_str().map(str::to_string))
        .collect();
    assert!(tags.contains(&super::MIGRATION_TAG.to_string()), "{tags:?}");
    assert!(
        tags.contains(&"name:arm-only-when-repo-gates-green".to_string()),
        "{tags:?}"
    );
    assert!(tags.contains(&"type:project".to_string()), "{tags:?}");

    // The facts moved; nothing was deleted.
    let archive = memory
        .parent()
        .expect("parent")
        .join("memory.archived-20260912");
    assert!(!memory.join("link-issues-and-prs.md").exists());
    assert!(archive.join("link-issues-and-prs.md").is_file());
    assert!(archive.join("arm-only-when-repo-gates-green.md").is_file());

    // The index is empty in place, with its former contents archived.
    assert_eq!(
        std::fs::read_to_string(memory.join(super::INDEX_FILE)).expect("read index"),
        ""
    );
    assert_eq!(
        std::fs::read_to_string(archive.join(super::INDEX_FILE)).expect("read archived index"),
        INDEX
    );
}

#[tokio::test]
async fn auto_memory_import_is_idempotent() {
    let project = tempfile::tempdir().expect("tempdir");
    let (config, _memory) = write_store(project.path(), &[("link-issues-and-prs.md", FACT)], INDEX);
    let (daemon, state) = start_stub().await;

    let first = run_auto_memory_import(&opts(project.path(), config.path(), daemon.socket()))
        .await
        .expect("first run");
    assert_eq!(first.stored, 1);

    let second = run_auto_memory_import(&opts(project.path(), config.path(), daemon.socket()))
        .await
        .expect("second run");
    assert_eq!(second.total, 0, "{:#?}", second.files);
    assert_eq!(second.stored, 0);
    assert!(!second.index_cleared);
    assert_eq!(
        state.lock().expect("lock").writes.len(),
        1,
        "a second run must issue no further memory_remember calls"
    );
}

#[tokio::test]
async fn auto_memory_import_leaves_a_failed_file_in_place() {
    // Fail-Open Check: a store that did not happen must not archive the file,
    // and must not let the index — which still names it — be emptied.
    let project = tempfile::tempdir().expect("tempdir");
    let (config, memory) = write_store(project.path(), &[("link-issues-and-prs.md", FACT)], INDEX);
    let (daemon, state) = start_stub().await;
    state
        .lock()
        .expect("lock")
        .deny
        .insert("memory_remember".to_string());

    let report = run_auto_memory_import(&opts(project.path(), config.path(), daemon.socket()))
        .await
        .expect("migration runs");

    assert_eq!(report.total, 1);
    assert_eq!(report.stored, 0);
    assert_eq!(report.failed, 1);
    assert_eq!(report.files[0].status, AutoImportStatus::Failed);
    assert!(report.files[0].error.is_some());
    assert!(!report.index_cleared);

    assert!(
        memory.join("link-issues-and-prs.md").is_file(),
        "a failed store must leave its file exactly where it was"
    );
    assert!(
        !memory
            .parent()
            .expect("parent")
            .join("memory.archived-20260912")
            .exists(),
        "nothing was stored, so nothing may be archived"
    );
    assert_eq!(
        std::fs::read_to_string(memory.join(super::INDEX_FILE)).expect("read index"),
        INDEX,
        "the index still names a file that is still on disk"
    );
}

#[tokio::test]
async fn auto_memory_import_retries_only_the_archive_after_a_rename_failure() {
    // #7685 r3: "stored" and "archived" are two states. A store that lands
    // followed by an archive rename that fails used to leave the fact file on
    // disk with no record of its drawer, so the next run stored it AGAIN and the
    // palace ended up with two drawers for one fact.
    //
    // The rename is forced to fail by putting a FILE where the archive DIRECTORY
    // must be: `create_dir_all` then fails deterministically, with no dependence
    // on permission bits or on the platform's rename semantics.
    let project = tempfile::tempdir().expect("tempdir");
    let (config, memory) = write_store(project.path(), &[("link-issues-and-prs.md", FACT)], INDEX);
    let archive = memory
        .parent()
        .expect("parent")
        .join("memory.archived-20260912");
    std::fs::write(&archive, "not a directory").expect("block the archive path");
    let (daemon, state) = start_stub().await;
    let fact = memory.join("link-issues-and-prs.md");

    let first = run_auto_memory_import(&opts(project.path(), config.path(), daemon.socket()))
        .await
        .expect("first run");

    assert_eq!(first.failed, 1, "{:#?}", first.files);
    assert_eq!(
        first.files[0].drawer_id.as_deref(),
        Some("drawer-1"),
        "a failed archive must still report the drawer the store produced"
    );
    assert!(fact.is_file(), "the unfiled fact stays on disk");
    assert!(!first.index_cleared);

    // Unblock the archive and re-run. The fact must be filed WITHOUT a second
    // store.
    std::fs::remove_file(&archive).expect("unblock");

    let second = run_auto_memory_import(&opts(project.path(), config.path(), daemon.socket()))
        .await
        .expect("second run");

    assert_eq!(second.stored, 1, "{:#?}", second.files);
    assert_eq!(
        second.files[0].drawer_id.as_deref(),
        Some("drawer-1"),
        "the retry must reuse the drawer the first run created"
    );
    assert_eq!(
        state.lock().expect("lock").writes.len(),
        1,
        "the fact must be stored exactly once across both runs"
    );
    assert!(archive.join("link-issues-and-prs.md").is_file());
    assert!(!fact.exists());
    assert!(
        !memory.join("link-issues-and-prs.md.stored").exists(),
        "the marker has nothing left to prove once the fact is filed"
    );
    assert!(second.index_cleared, "{:#?}", second);
}

#[tokio::test]
async fn auto_memory_import_on_an_absent_store_is_a_no_op() {
    let project = tempfile::tempdir().expect("tempdir");
    let config = tempfile::tempdir().expect("tempdir");
    let (daemon, state) = start_stub().await;

    let report = run_auto_memory_import(&opts(project.path(), config.path(), daemon.socket()))
        .await
        .expect("an absent store is not an error");

    assert_eq!(report.total, 0);
    assert!(report.archive.is_none());
    assert!(state.lock().expect("lock").writes.is_empty());
}

#[test]
fn import_key_is_deterministic_per_path_and_content() {
    // #7685: a retry can only find a crashed run's drawer by a key both runs
    // derive identically from what is on disk.
    let a = super::import_key(Path::new("/m/a.md"), FACT);
    assert_eq!(a, super::import_key(Path::new("/m/a.md"), FACT));
    assert!(a.starts_with(super::IMPORT_KEY_PREFIX), "{a}");
    assert_ne!(a, super::import_key(Path::new("/m/b.md"), FACT));
    assert_ne!(a, super::import_key(Path::new("/m/a.md"), OTHER_FACT));
}

#[tokio::test]
async fn auto_memory_import_never_duplicates_a_fact_whose_marker_was_lost() {
    // #7685 r4: the crash window. `memory_remember` succeeds, then the `.stored`
    // sidecar never lands — here forced by a DIRECTORY where the marker file must
    // go, which is what a kill between the two calls leaves behind. The re-run
    // finds no marker and must still not store the fact a second time.
    let project = tempfile::tempdir().expect("tempdir");
    let (config, memory) = write_store(project.path(), &[("link-issues-and-prs.md", FACT)], INDEX);
    let marker = memory.join("link-issues-and-prs.md.stored");
    std::fs::create_dir(&marker).expect("block the marker path");
    let (daemon, state) = start_stub().await;

    let first = run_auto_memory_import(&opts(project.path(), config.path(), daemon.socket()))
        .await
        .expect("first run");
    assert_eq!(first.failed, 1, "{:#?}", first.files);
    assert_eq!(state.lock().expect("lock").writes.len(), 1);

    // The "crash": the marker never landed.
    std::fs::remove_dir(&marker).expect("unblock");

    let second = run_auto_memory_import(&opts(project.path(), config.path(), daemon.socket()))
        .await
        .expect("second run");

    assert_eq!(
        state.lock().expect("lock").writes.len(),
        1,
        "exactly one stored fact across the crash and the re-run"
    );
    assert_eq!(second.stored, 1, "{:#?}", second.files);
    assert_eq!(
        second.files[0].drawer_id.as_deref(),
        Some("drawer-1"),
        "the re-run must reuse the drawer the crashed run wrote"
    );
    assert!(second.index_cleared, "{second:#?}");
}

#[tokio::test]
async fn auto_memory_import_does_not_archive_when_the_stored_drawer_is_gone() {
    // #7752: the marker records that a store SUCCEEDED, never that the drawer
    // still exists. A drawer deleted after the marker landed used to archive the
    // fact file anyway — and the archive move is what takes the fact out of the
    // live set, so the content then survived nowhere.
    //
    // The setup is the rename-failure one: a FILE where the archive DIRECTORY
    // must be leaves the fact on disk with its marker written. The palace then
    // forgets the drawer the marker names.
    let project = tempfile::tempdir().expect("tempdir");
    let (config, memory) = write_store(project.path(), &[("link-issues-and-prs.md", FACT)], INDEX);
    let archive = memory
        .parent()
        .expect("parent")
        .join("memory.archived-20260912");
    std::fs::write(&archive, "not a directory").expect("block the archive path");
    let (daemon, state) = start_stub().await;
    let fact = memory.join("link-issues-and-prs.md");
    let marker = memory.join("link-issues-and-prs.md.stored");

    let first = run_auto_memory_import(&opts(project.path(), config.path(), daemon.socket()))
        .await
        .expect("first run");
    assert_eq!(first.failed, 1, "{:#?}", first.files);
    assert_eq!(marker_id(&marker), Some("drawer-1".to_string()));

    // The deletion the marker cannot see, and an archive path that would now
    // work — so the only thing standing between the file and the archive is the
    // verification.
    std::fs::remove_file(&archive).expect("unblock");
    state
        .lock()
        .expect("lock")
        .forgotten
        .insert("drawer-1".to_string());

    let second = run_auto_memory_import(&opts(project.path(), config.path(), daemon.socket()))
        .await
        .expect("second run");

    assert_eq!(second.stored, 0, "{:#?}", second.files);
    assert_eq!(second.failed, 1, "{:#?}", second.files);
    assert!(
        fact.is_file(),
        "a fact whose drawer is gone must stay in the live set"
    );
    assert!(
        !archive.join("link-issues-and-prs.md").exists(),
        "archiving it would leave the content nowhere"
    );
    assert!(
        !second.index_cleared,
        "the index still names a file that is still on disk: {second:#?}"
    );
    let error = second.files[0].error.clone().unwrap_or_default();
    assert!(error.contains("drawer-1"), "{error}");
    assert!(error.contains("no longer holds it"), "{error}");
    assert!(
        !marker.exists(),
        "the stale marker is cleared, so the next run takes the ordinary store path"
    );
    assert_eq!(
        state.lock().expect("lock").writes.len(),
        1,
        "a drawer the operator may have deleted on purpose is not re-stored behind their back"
    );

    // Recovery: with the marker gone, the next run imports the fact again.
    let third = run_auto_memory_import(&opts(project.path(), config.path(), daemon.socket()))
        .await
        .expect("third run");
    assert_eq!(third.stored, 1, "{:#?}", third.files);
    assert_eq!(third.files[0].drawer_id.as_deref(), Some("drawer-2"));
    assert!(archive.join("link-issues-and-prs.md").is_file());
    assert!(third.index_cleared, "{third:#?}");
}

#[tokio::test]
async fn auto_memory_import_leaves_the_file_alone_when_the_palace_cannot_be_reached() {
    // #7752 Fail-Open Check: a palace that cannot answer proves nothing either
    // way, so the marker stands and the file does not move. Archiving on an
    // unanswered lookup would be the same data loss as archiving on a stale one.
    let project = tempfile::tempdir().expect("tempdir");
    let (config, memory) = write_store(project.path(), &[("link-issues-and-prs.md", FACT)], INDEX);
    let fact = memory.join("link-issues-and-prs.md");
    let marker = memory.join("link-issues-and-prs.md.stored");
    std::fs::write(&marker, "drawer-1").expect("write marker");
    let (daemon, state) = start_stub().await;
    state
        .lock()
        .expect("lock")
        .deny
        .insert("memory_list".to_string());

    let report = run_auto_memory_import(&opts(project.path(), config.path(), daemon.socket()))
        .await
        .expect("migration runs");

    assert_eq!(report.stored, 0, "{:#?}", report.files);
    assert_eq!(report.failed, 1, "{:#?}", report.files);
    assert_eq!(
        report.files[0].drawer_id.as_deref(),
        Some("drawer-1"),
        "the unverified drawer id is still reported: {:#?}",
        report.files
    );
    assert!(fact.is_file(), "an unverifiable marker archives nothing");
    assert!(
        !memory
            .parent()
            .expect("parent")
            .join("memory.archived-20260912")
            .exists()
    );
    assert_eq!(
        marker_id(&marker),
        Some("drawer-1".to_string()),
        "an unanswered lookup is not proof the drawer is gone, so the marker stands"
    );
    assert!(!report.index_cleared);
    assert!(
        state.lock().expect("lock").writes.is_empty(),
        "an unverifiable marker must not trigger a second store either"
    );
}

/// The drawer id a `.stored` marker names, if the marker is there.
fn marker_id(marker: &Path) -> Option<String> {
    std::fs::read_to_string(marker)
        .ok()
        .map(|t| t.trim().to_string())
}
