//! Read compatibility with the pre-rename locations, and the one-time move
//! (#11632, MB1516 §11).
//!
//! Why: four assistants exist on the operator's machine under
//! `~/trusty-agents/<id>/` (`assistant`, `cto-assistant`, `izzie`,
//! `writing-assistant`), with config in `~/.trusty-agents`. The rename must
//! keep reading them and then move them without losing or overwriting
//! anything, so those four ids are the fixtures here.
//! What: one test per old location read, the move itself, the
//! both-exist refusal, and two failed-move arms.
//! Test: this module IS the test surface.

use std::path::{Path, PathBuf};

use serial_test::serial;

use super::home_tests::EnvVarGuard;
use crate::assistants::{
    APP_DIR_NAME, ASSISTANTS_DIR_ENV, ASSISTANTS_DIR_NAME, AssistantError, AssistantHome,
    AssistantInstanceId, LEGACY_APP_DIR_NAME, LEGACY_ASSISTANTS_DIR_ENV,
    LEGACY_ASSISTANTS_DIR_NAME, app_dir, assistants_root, migrate_legacy_locations,
    resolve_assistants_root,
};
use crate::knowledge::KnowledgeStore;

const PILOTS: [&str; 4] = ["assistant", "cto-assistant", "izzie", "writing-assistant"];

/// A canonical temp dir: the knowledge store refuses a path with a symlink
/// in it, and macOS temp dirs live under the `/var` symlink.
fn canonical_tmp() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().canonicalize().expect("canonicalize");
    (tmp, path)
}

/// Create `<root>/<id>/instructions.md` holding `body`.
fn seed_home(root: &Path, id: &str, body: &str) -> PathBuf {
    let dir = root.join(id);
    std::fs::create_dir_all(&dir).expect("mkdir home");
    std::fs::write(dir.join("instructions.md"), body).expect("write instructions");
    dir
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
}

/// Why: `TAGENT_ASSISTANTS_DIR` keeps working for one release, and the new
/// `TA_ASSISTANTS_DIR` wins when both are set.
/// Test: itself.
#[test]
#[serial]
fn new_variable_wins_and_the_old_one_is_still_read() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let old = tmp.path().join("old-root");
    let new = tmp.path().join("new-root");
    let _no_new = EnvVarGuard::clear(ASSISTANTS_DIR_ENV);
    let _old = EnvVarGuard::set(LEGACY_ASSISTANTS_DIR_ENV, &old);
    assert_eq!(assistants_root().expect("old variable"), old);

    let _new = EnvVarGuard::set(ASSISTANTS_DIR_ENV, &new);
    assert_eq!(assistants_root().expect("new variable"), new);
}

/// Why: a home under `~/trusty-agents/<id>/` must load before the move runs,
/// and the new root takes over as soon as it exists.
/// Test: itself.
#[test]
#[serial]
fn legacy_homes_root_is_read_until_the_new_one_exists() {
    let _home_lock = crate::test_env::HOME_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().expect("tempdir");
    let legacy = tmp.path().join(LEGACY_ASSISTANTS_DIR_NAME);
    seed_home(&legacy, "izzie", "izzie, before the rename");
    let _no_new = EnvVarGuard::clear(ASSISTANTS_DIR_ENV);
    let _no_old = EnvVarGuard::clear(LEGACY_ASSISTANTS_DIR_ENV);
    let _home = EnvVarGuard::set("HOME", tmp.path());

    let home = AssistantHome::for_instance("izzie").expect("legacy home");
    assert_eq!(home.path(), legacy.join("izzie"));
    assert_eq!(read(&home.instructions_path()), "izzie, before the rename");

    std::fs::create_dir_all(tmp.path().join(ASSISTANTS_DIR_NAME)).expect("mkdir new root");
    assert_eq!(
        resolve_assistants_root(tmp.path()),
        tmp.path().join(ASSISTANTS_DIR_NAME)
    );
}

/// Why: provider policy, the user profile and per-user state live in the app
/// directory; `~/.trusty-agents` must keep loading until it moves.
/// Test: itself.
#[test]
fn legacy_app_dir_is_read_until_the_new_one_exists() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let legacy = tmp.path().join(LEGACY_APP_DIR_NAME);
    std::fs::create_dir_all(&legacy).expect("mkdir");
    std::fs::write(
        legacy.join("config.toml"),
        "[providers]\ndefault_provider_id = \"bedrock\"\n",
    )
    .expect("write config");

    assert_eq!(app_dir(tmp.path()), legacy);
    assert_eq!(
        crate::llm::provider_policy::read_from_home(tmp.path()).as_deref(),
        Some("bedrock")
    );

    std::fs::create_dir_all(tmp.path().join(APP_DIR_NAME)).expect("mkdir new");
    assert_eq!(app_dir(tmp.path()), tmp.path().join(APP_DIR_NAME));
}

/// Why: the move is what lets the next release drop the read path. Each pilot
/// home, the files beside them (the operator's root is a git checkout) and
/// the app directory must land under the new name with their content, a
/// home's recorded OKG root must follow it, and the old path must still
/// resolve for absolute paths stored elsewhere.
/// Test: itself.
#[test]
#[serial]
fn moves_each_pilot_home_once() {
    let (_tmp, home) = canonical_tmp();
    let _no_new = EnvVarGuard::clear(ASSISTANTS_DIR_ENV);
    let _no_old = EnvVarGuard::clear(LEGACY_ASSISTANTS_DIR_ENV);
    let legacy_root = home.join(LEGACY_ASSISTANTS_DIR_NAME);
    for id in PILOTS {
        seed_home(&legacy_root, id, id);
    }
    std::fs::create_dir_all(legacy_root.join(".git")).expect("mkdir .git");
    std::fs::write(legacy_root.join("README.md"), "root readme").expect("write readme");
    let legacy_izzie =
        AssistantHome::under(&legacy_root, AssistantInstanceId::new("izzie").unwrap());
    let before = KnowledgeStore::new(legacy_izzie.clone())
        .initialize(chrono::Utc::now(), None)
        .expect("initialize knowledge state");
    assert_eq!(before.store.root, legacy_izzie.okg_dir());
    std::fs::create_dir_all(home.join(LEGACY_APP_DIR_NAME)).expect("mkdir app dir");
    std::fs::write(
        home.join(LEGACY_APP_DIR_NAME).join("user.toml"),
        "name = \"Ada\"\n",
    )
    .expect("write profile");

    let report = migrate_legacy_locations(&home).expect("move");

    let new_root = home.join(ASSISTANTS_DIR_NAME);
    assert_eq!(
        report.moved,
        vec![
            (home.join(LEGACY_APP_DIR_NAME), home.join(APP_DIR_NAME)),
            (legacy_root.clone(), new_root.clone()),
        ]
    );
    assert!(report.kept_both.is_empty(), "{report:?}");
    assert!(
        is_symlink(&legacy_root),
        "the old root is left as a pointer"
    );
    for id in PILOTS {
        assert_eq!(read(&new_root.join(id).join("instructions.md")), id);
        assert_eq!(
            legacy_root.join(id).canonicalize().unwrap(),
            new_root.join(id)
        );
    }
    assert!(new_root.join(".git").is_dir());
    assert_eq!(read(&new_root.join("README.md")), "root readme");
    assert_eq!(
        read(&home.join(APP_DIR_NAME).join("user.toml")),
        "name = \"Ada\"\n"
    );
    assert!(is_symlink(&home.join(LEGACY_APP_DIR_NAME)));
    let moved_izzie = AssistantHome::under(&new_root, AssistantInstanceId::new("izzie").unwrap());
    let after = KnowledgeStore::new(moved_izzie.clone())
        .status()
        .expect("moved knowledge state validates against the new home")
        .expect("state present");
    assert_eq!(after.store.root, moved_izzie.okg_dir());
    assert_eq!(after.store.index_id, before.store.index_id);
    assert_eq!(resolve_assistants_root(&home), new_root);
    assert_eq!(app_dir(&home), home.join(APP_DIR_NAME));

    let again = migrate_legacy_locations(&home).expect("second run");
    assert_eq!(again, Default::default(), "the move runs once");
}

/// Why: a home at the new location is never overwritten. When both roots
/// exist, both stay exactly as they are, the new one is read, and the caller
/// is told to merge by hand.
/// Test: itself.
#[test]
#[serial]
fn both_homes_present_keeps_both_untouched() {
    let (_tmp, home) = canonical_tmp();
    let _no_new = EnvVarGuard::clear(ASSISTANTS_DIR_ENV);
    let _no_old = EnvVarGuard::clear(LEGACY_ASSISTANTS_DIR_ENV);
    let legacy_root = home.join(LEGACY_ASSISTANTS_DIR_NAME);
    let new_root = home.join(ASSISTANTS_DIR_NAME);
    let old_izzie = seed_home(&legacy_root, "izzie", "old izzie");
    let new_izzie = seed_home(&new_root, "izzie", "new izzie");
    seed_home(&legacy_root, "cto-assistant", "cto");

    let report = migrate_legacy_locations(&home).expect("move");

    assert_eq!(
        report.kept_both,
        vec![(legacy_root.clone(), new_root.clone())]
    );
    assert!(report.moved.is_empty(), "{report:?}");
    assert!(!is_symlink(&legacy_root), "the old root is left as it was");
    assert_eq!(
        read(&legacy_root.join("cto-assistant/instructions.md")),
        "cto"
    );
    assert_eq!(resolve_assistants_root(&home), new_root);
    assert_eq!(read(&old_izzie.join("instructions.md")), "old izzie");
    assert_eq!(read(&new_izzie.join("instructions.md")), "new izzie");
    assert!(
        report.lines().iter().any(|l| l.starts_with("both ")),
        "{:?}",
        report.lines()
    );
}

/// Why: the Fail-Open Check — a move that cannot complete must leave the old
/// home where the read path finds it, and say so with an error.
/// Test: itself.
#[test]
#[serial]
fn failed_move_leaves_the_legacy_home_in_place() {
    let (_tmp, home) = canonical_tmp();
    let _no_new = EnvVarGuard::clear(ASSISTANTS_DIR_ENV);
    let _no_old = EnvVarGuard::clear(LEGACY_ASSISTANTS_DIR_ENV);
    let legacy_root = home.join(LEGACY_ASSISTANTS_DIR_NAME);
    let legacy_home = seed_home(&legacy_root, "writing-assistant", "drafts");
    // A read-only parent refuses the rename.
    let _writable = ReadOnly::new(&home);

    let err = migrate_legacy_locations(&home).expect_err("the move must fail");

    assert!(matches!(err, AssistantError::Relocate { .. }), "{err}");
    assert!(!is_symlink(&legacy_root));
    assert_eq!(read(&legacy_home.join("instructions.md")), "drafts");
    assert!(!home.join(ASSISTANTS_DIR_NAME).exists());
    assert_eq!(resolve_assistants_root(&home), legacy_root);
}

/// Why: the Fail-Open Check on the undo path — a failure after the rename
/// must put the directory back and restore every state file, not leave a
/// moved tree whose recorded roots point at the old path.
/// Test: itself.
#[test]
#[serial]
fn failed_state_rewrite_undoes_the_move() {
    let (_tmp, home) = canonical_tmp();
    let _no_new = EnvVarGuard::clear(ASSISTANTS_DIR_ENV);
    let _no_old = EnvVarGuard::clear(LEGACY_ASSISTANTS_DIR_ENV);
    let legacy_root = home.join(LEGACY_ASSISTANTS_DIR_NAME);
    let legacy_izzie = AssistantHome::under(
        &legacy_root,
        AssistantInstanceId::new("izzie").expect("valid id"),
    );
    seed_home(&legacy_root, "izzie", "izzie");
    KnowledgeStore::new(legacy_izzie.clone())
        .initialize(chrono::Utc::now(), None)
        .expect("initialize knowledge state");
    let state_dir = legacy_izzie.path().join("stores/knowledge-pipeline");
    let before = read(&state_dir.join("state.json"));
    // The state file can be read but not replaced: the rewrite fails after
    // the rename has already happened.
    let read_only = ReadOnly::new(&state_dir);

    let err = migrate_legacy_locations(&home).expect_err("the rewrite must fail");
    drop(read_only);

    assert!(err.to_string().contains("the move was undone"), "{err}");
    assert!(!is_symlink(&legacy_root));
    assert!(!home.join(ASSISTANTS_DIR_NAME).exists());
    assert_eq!(read(&state_dir.join("state.json")), before);
    KnowledgeStore::new(legacy_izzie)
        .status()
        .expect("the state still validates at the old home");
}

/// Makes a directory read-only for one test body, and writable again on drop
/// so the temp dir can be removed.
struct ReadOnly(PathBuf);

impl ReadOnly {
    fn new(dir: &Path) -> Self {
        set_mode(dir, 0o555);
        Self(dir.to_path_buf())
    }
}

impl Drop for ReadOnly {
    fn drop(&mut self) {
        set_mode(&self.0, 0o755);
    }
}

fn set_mode(dir: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode)).expect("chmod");
}

/// Why: a knowledge state the move cannot rewrite would fail validation at
/// the new path, so the move is refused before anything changes.
/// Test: itself.
#[test]
#[serial]
fn unreadable_knowledge_state_refuses_the_move() {
    let (_tmp, home) = canonical_tmp();
    let _no_new = EnvVarGuard::clear(ASSISTANTS_DIR_ENV);
    let _no_old = EnvVarGuard::clear(LEGACY_ASSISTANTS_DIR_ENV);
    let legacy_root = home.join(LEGACY_ASSISTANTS_DIR_NAME);
    let legacy_home = seed_home(&legacy_root, "assistant", "assistant");
    let state = legacy_home.join("stores/knowledge-pipeline/state.json");
    std::fs::create_dir_all(state.parent().unwrap()).expect("mkdir");
    std::fs::write(&state, "not json").expect("write state");

    let err = migrate_legacy_locations(&home).expect_err("the move must fail");

    assert!(err.to_string().contains("state.json"), "{err}");
    assert!(!is_symlink(&legacy_home));
    assert_eq!(read(&state), "not json");
    assert!(!home.join(ASSISTANTS_DIR_NAME).join("assistant").exists());
}
