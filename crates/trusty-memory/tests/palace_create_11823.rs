//! Regression tests for the daemon's palace-create path (#11823).
//!
//! Why: the daemon validated a create against the folder name, fell back to
//! its own cwd and environment, and overwrote an existing palace's metadata.
//! This binary never sets `TRUSTY_SKIP_PALACE_ENFORCEMENT`, so the real gate
//! runs; every test is `#[serial]` because one of them sets
//! `TRUSTY_MEMORY_PALACE` for the whole process.
//! What: drives the MCP `palace_create` tool and `MemoryService::create_palace`
//! against tempdir projects and a tempdir data root.
//! Test: this IS the test module.

use std::path::Path;
use std::process::Command;

use serde_json::{Value, json};
use serial_test::serial;
use tempfile::TempDir;
use trusty_common::palace_alias::PalaceAliasStore;
use trusty_memory::ActivitySource;
use trusty_memory::AppState;
use trusty_memory::project_root::resolve_palace_for_project;
use trusty_memory::service::{CreatePalaceBody, MemoryService, ServiceError};
use trusty_memory::tools::dispatch_tool;

/// A ready `AppState` rooted at a fresh data dir, plus a projects dir.
fn fixture() -> (AppState, TempDir, TempDir) {
    let data = tempfile::tempdir().expect("data root");
    let projects = tempfile::tempdir().expect("projects dir");
    let state = AppState::new(data.path().to_path_buf());
    state.set_ready();
    (state, data, projects)
}

/// `projects/work/<leaf>` with a `.git` marker and no remote: it resolves to
/// `work-<leaf>`.
fn plain_project(projects: &Path, leaf: &str) -> std::path::PathBuf {
    let root = projects.join("work").join(leaf);
    std::fs::create_dir_all(root.join(".git")).expect("project marker");
    root
}

/// A real git repo at `projects/<dir>` with `origin` set to `remote`.
fn git_project(projects: &Path, dir: &str, remote: &str) -> std::path::PathBuf {
    let root = projects.join(dir);
    std::fs::create_dir_all(&root).expect("repo dir");
    for args in [vec!["init", "-q"], vec!["remote", "add", "origin", remote]] {
        let out = Command::new("git")
            .args(&args)
            .current_dir(&root)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .output()
            .expect("run git");
        assert!(out.status.success(), "git {args:?}: {out:?}");
    }
    root
}

async fn create(state: &AppState, args: Value) -> anyhow::Result<Value> {
    dispatch_tool(state, "palace_create", args).await
}

/// Why (#11823, Q5): a create with no project directory used the daemon's own
/// cwd, then the data root, so the name was checked against the wrong project.
#[tokio::test]
#[serial]
async fn create_without_cwd_is_rejected() {
    let (state, data, _projects) = fixture();
    let err = create(&state, json!({"name": "some-project"}))
        .await
        .expect_err("a create with no cwd must be refused");
    assert!(format!("{err:#}").contains("`cwd` is required"), "{err:#}");
    assert!(!data.path().join("some-project").exists());
}

/// Why (#11823, Q5): the service surface (formerly HTTP) answers a 4xx.
#[tokio::test]
#[serial]
async fn service_create_without_cwd_is_a_bad_request() {
    let (state, data, _projects) = fixture();
    let svc = MemoryService::new(state);
    let body = CreatePalaceBody {
        name: "some-project".to_string(),
        description: None,
        cwd: None,
        force: false,
    };
    match svc.create_palace(body, ActivitySource::Http).await {
        Err(ServiceError::BadRequest(msg)) => assert!(msg.contains("`cwd` is required"), "{msg}"),
        other => panic!("expected BadRequest, got {other:?}"),
    }
    assert!(!data.path().join("some-project").exists());
}

/// Why (#11823, Q2): a create for an existing palace rewrote its
/// `palace.json`. It now answers `exists` and leaves the file byte-identical,
/// including under `force=true`.
#[tokio::test]
#[serial]
async fn create_on_an_existing_palace_answers_exists_and_leaves_it_untouched() {
    let (state, data, projects) = fixture();
    let cwd = plain_project(projects.path(), "proj");
    let cwd = cwd.to_string_lossy();
    let first = create(&state, json!({"name": "work-proj", "cwd": cwd}))
        .await
        .expect("first create");
    assert_eq!(first["status"], "created", "{first}");
    let path = data.path().join("work-proj").join("palace.json");
    let before = std::fs::read(&path).expect("read palace.json");

    for args in [
        json!({"name": "work-proj", "cwd": cwd, "description": "replacement"}),
        json!({"name": "work-proj", "force": true, "description": "replacement"}),
    ] {
        let again = create(&state, args).await.expect("repeat create");
        assert_eq!(again["status"], "exists", "{again}");
        assert_eq!(again["palace_id"], "work-proj", "{again}");
        assert!(again.get("alias_of").is_none(), "{again}");
    }
    assert_eq!(std::fs::read(&path).expect("read palace.json"), before);
}

/// Why (#11823, Q1): a project whose memory sits in a folder-named palace
/// (`my-repo`) resolves to `acme-my-repo`. The create links the two by alias —
/// no new palace, no rename — and a repeat answers `exists` through the alias.
#[tokio::test]
#[serial]
async fn create_aliases_the_folder_named_palace() {
    let (state, data, projects) = fixture();
    let root = git_project(
        projects.path(),
        "My_Repo",
        "git@github.com:Acme/My_Repo.git",
    );
    let cwd = root.to_string_lossy();
    create(&state, json!({"name": "my-repo", "force": true}))
        .await
        .expect("seed the folder-named palace");
    let folder_json = std::fs::read(data.path().join("my-repo").join("palace.json")).expect("read");

    let aliased = create(&state, json!({"name": "acme-my-repo", "cwd": cwd}))
        .await
        .expect("create the resolved name");
    assert_eq!(
        aliased,
        json!({"palace_id": "acme-my-repo", "status": "aliased", "alias_of": "my-repo", "bootstrap": null})
    );
    assert!(
        !data.path().join("acme-my-repo").exists(),
        "no new palace dir"
    );
    assert_eq!(
        std::fs::read(data.path().join("my-repo").join("palace.json")).expect("read"),
        folder_json
    );
    let target = PalaceAliasStore::resolve_alias(data.path(), "acme-my-repo").expect("read alias");
    assert_eq!(target.as_deref(), Some("my-repo"));

    let again = create(&state, json!({"name": "acme-my-repo", "cwd": cwd}))
        .await
        .expect("repeat create");
    assert_eq!(again["status"], "exists", "{again}");
    assert_eq!(again["alias_of"], "my-repo", "{again}");
}

/// Why (#11823): alias resolution is one hop. When the folder id is itself a
/// live alias of `x`, aliasing the resolved name to the folder id left a dead
/// alias while the create answered `aliased`.
/// What: folder palace `my-repo` is a live alias of real palace `real-x`; the
/// create must alias `acme-my-repo` straight to `real-x`, so it resolves.
#[tokio::test]
#[serial]
async fn create_aliases_through_a_folder_alias_to_its_target() {
    let (state, data, projects) = fixture();
    let root = git_project(
        projects.path(),
        "My_Repo",
        "git@github.com:Acme/My_Repo.git",
    );
    let cwd = root.to_string_lossy();
    create(&state, json!({"name": "real-x", "force": true}))
        .await
        .expect("seed the real palace");
    PalaceAliasStore::register_alias(data.path(), "my-repo", "real-x").expect("folder alias");

    let aliased = create(&state, json!({"name": "acme-my-repo", "cwd": cwd}))
        .await
        .expect("create the resolved name");
    assert_eq!(aliased["status"], "aliased", "{aliased}");
    assert_eq!(aliased["alias_of"], "real-x", "{aliased}");
    assert_eq!(
        trusty_common::palace_alias::alias_target_if_absent(data.path(), "acme-my-repo").as_deref(),
        Some("real-x"),
        "the resolved name must be servable"
    );
}

/// Why (#11823, Q2): the shared resolver reads `TRUSTY_MEMORY_PALACE` first,
/// and inside the daemon that is the daemon's variable, not the client's.
#[tokio::test]
#[serial]
async fn daemon_palace_env_does_not_change_a_client_create() {
    let (state, data, projects) = fixture();
    let cwd = plain_project(projects.path(), "envproj");
    let cwd = cwd.to_string_lossy().to_string();
    let _env = PalaceEnv::set("daemon-palace");

    let err = create(&state, json!({"name": "daemon-palace", "cwd": cwd}))
        .await
        .expect_err("the daemon's env must not name a client's palace");
    assert!(format!("{err:#}").contains("work-envproj"), "{err:#}");
    assert!(!data.path().join("daemon-palace").exists());
    let ok = create(&state, json!({"name": "work-envproj", "cwd": cwd}))
        .await
        .expect("the project's own name is accepted");
    assert_eq!(ok["status"], "created", "{ok}");
}

/// Why: the resolver the daemon validates with ignores the process env.
#[test]
#[serial]
fn resolver_ignores_the_process_palace_env() {
    let projects = tempfile::tempdir().expect("projects dir");
    let root = plain_project(projects.path(), "quiet");
    let _env = PalaceEnv::set("daemon-palace");
    let project = resolve_palace_for_project(&root).expect("resolve");
    assert_eq!(project.id, "work-quiet");
}

/// Why (#11823): a malformed pin blocks the create and is reported; there is
/// no fallback name and nothing is created.
#[tokio::test]
#[serial]
async fn create_with_a_malformed_pin_is_rejected() {
    let (state, data, projects) = fixture();
    let root = plain_project(projects.path(), "badpin");
    std::fs::create_dir_all(root.join(".trusty-tools")).expect("pin dir");
    std::fs::write(
        root.join(".trusty-tools/trusty-memory.yaml"),
        "palace: [not, a, string]\n",
    )
    .expect("pin");
    let cwd = root.to_string_lossy();
    for name in ["work-badpin", "badpin"] {
        let err = create(&state, json!({"name": name, "cwd": cwd}))
            .await
            .expect_err("a malformed pin must refuse the create");
        assert!(format!("{err:#}").contains("trusty-memory.yaml"), "{err:#}");
        assert!(!data.path().join(name).exists());
    }
}

/// Sets `TRUSTY_MEMORY_PALACE` and removes it on drop.
struct PalaceEnv;

impl PalaceEnv {
    fn set(value: &str) -> Self {
        // SAFETY: every test in this binary is `#[serial]`, so no other thread
        // of this process reads or writes the environment meanwhile.
        unsafe { std::env::set_var("TRUSTY_MEMORY_PALACE", value) };
        Self
    }
}

impl Drop for PalaceEnv {
    fn drop(&mut self) {
        // SAFETY: as in `set`.
        unsafe { std::env::remove_var("TRUSTY_MEMORY_PALACE") };
    }
}
