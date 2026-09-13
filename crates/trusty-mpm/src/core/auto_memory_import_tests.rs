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
