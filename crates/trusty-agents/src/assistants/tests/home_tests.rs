//! Layout, resolution and app-generated creation of an assistant home (#4325).
//!
//! Why: the layout is a product decision the ticket states literally (five
//! entries, dotless, TOML config), and `ensure` is the "AUTO-CREATED,
//! APP-GENERATED" half — its additive-only behaviour is what makes external
//! edits safe, so it is pinned rather than assumed.
//! What: layout accessors, root resolution (default + env override), and the
//! three `ensure` properties: it creates everything, it repeats nothing, and it
//! overwrites nothing.
//! Test: this module IS the test surface.

use std::path::{Path, PathBuf};

use serial_test::serial;

use crate::assistants::{
    AGENTS_DIR, ASSISTANTS_DIR_ENV, ASSISTANTS_DIR_NAME, ATTACHMENTS_DIR, AssistantHome,
    AssistantHomeConfig, AssistantInstanceId, CONFIG_FILE, INSTRUCTIONS_FILE, OKG_DIR,
    assistants_root,
};

/// RAII guard setting or clearing a process env var for one test body.
///
/// Mirrors the crate's established `#[serial]` + guard convention (see
/// `workflow::engine::executor::tests::checkpoint_resume`).
pub(super) struct EnvVarGuard {
    key: &'static str,
    prev: Option<String>,
}

impl EnvVarGuard {
    pub(super) fn set(key: &'static str, value: &Path) -> Self {
        let prev = std::env::var(key).ok();
        // SAFETY: serialized against other env-mutating tests by `#[serial]`.
        unsafe { std::env::set_var(key, value) };
        Self { key, prev }
    }

    pub(super) fn clear(key: &'static str) -> Self {
        let prev = std::env::var(key).ok();
        // SAFETY: as above.
        unsafe { std::env::remove_var(key) };
        Self { key, prev }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        unsafe {
            match &self.prev {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

/// A home for `izzie` under a throwaway root.
pub(super) fn temp_home() -> (tempfile::TempDir, AssistantHome) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let id = AssistantInstanceId::new("izzie").expect("valid id");
    // A throwaway root standing in for `~/trusty-agents`; the real spelling is
    // pinned by `okg_store_path_matches_the_owners_spelling`.
    let home = AssistantHome::under(tmp.path().join("trusty-agents"), id);
    (tmp, home)
}

/// Why: #4325 names the five entries literally; this is the layout contract
/// every later issue (#4282, #4283) resolves against.
#[test]
fn layout_matches_the_ticket() {
    let (_tmp, home) = temp_home();
    let root = home.path().to_path_buf();
    assert!(root.ends_with("izzie"), "home is <root>/<instance>");
    assert_eq!(home.id().as_str(), "izzie");
    assert_eq!(home.instructions_path(), root.join(INSTRUCTIONS_FILE));
    assert_eq!(home.config_path(), root.join(CONFIG_FILE));
    assert_eq!(home.agents_dir(), root.join(AGENTS_DIR));
    assert_eq!(home.okg_dir(), root.join(OKG_DIR));
    assert_eq!(home.attachments_dir(), root.join(ATTACHMENTS_DIR));
    // Constructing a home must not touch the filesystem.
    assert!(!home.exists());
}

/// Why: two instances of the same TYPE must not share a directory — that is
/// the whole point of instance isolation.
#[test]
fn two_instances_get_separate_homes() {
    let root = PathBuf::from("/trusty-agents");
    let izzie = AssistantHome::under(&root, AssistantInstanceId::new("izzie").unwrap());
    let cto = AssistantHome::under(&root, AssistantInstanceId::new("cto-assistant").unwrap());
    assert_ne!(izzie.path(), cto.path());
    assert_ne!(izzie.okg_dir(), cto.okg_dir());
    assert_eq!(izzie.okg_dir(), root.join("izzie").join(OKG_DIR));
    assert_eq!(cto.okg_dir(), root.join("cto-assistant").join(OKG_DIR));
}

#[test]
fn for_instance_validates_the_id() {
    let tmp = tempfile::tempdir().unwrap();
    let _guard = EnvVarGuard::set(ASSISTANTS_DIR_ENV, tmp.path());
    assert!(AssistantHome::for_instance("../escape").is_err());
    let home = AssistantHome::for_instance("izzie").expect("valid id");
    assert_eq!(home.id().as_str(), "izzie");
    assert_eq!(home.path(), tmp.path().join("izzie"));
}

/// Why: DOTLESS is a confirmed product decision (#4325 "Open Questions
/// Resolved"), so a stray leading dot is a regression, not a style change.
#[test]
#[serial]
fn default_root_is_dotless_under_the_user_home() {
    let tmp = tempfile::tempdir().unwrap();
    let _no_override = EnvVarGuard::clear(ASSISTANTS_DIR_ENV);
    let _home = EnvVarGuard::set("HOME", tmp.path());
    let root = assistants_root().expect("HOME is set");
    assert_eq!(root, tmp.path().join(ASSISTANTS_DIR_NAME));
    assert!(
        !ASSISTANTS_DIR_NAME.starts_with('.'),
        "the assistants root must be visible, not hidden"
    );
}

/// Why: the owner specified the store path literally on 2026-08-01 —
/// `trusty-agents/<agent>/okg/`. An intervening `assistants/` segment (the
/// spelling this PR originally carried) would make every path the OKG Sources
/// spec is written against wrong, so the full absolute path is pinned here
/// rather than only the accessors.
#[test]
#[serial]
fn okg_store_path_matches_the_owners_spelling() {
    let tmp = tempfile::tempdir().unwrap();
    let _no_override = EnvVarGuard::clear(ASSISTANTS_DIR_ENV);
    let _home = EnvVarGuard::set("HOME", tmp.path());

    let home = AssistantHome::for_instance("izzie").expect("valid id");

    assert_eq!(home.path(), tmp.path().join("trusty-agents").join("izzie"));
    assert_eq!(
        home.okg_dir(),
        tmp.path().join("trusty-agents").join("izzie").join("okg")
    );
}

#[test]
#[serial]
fn env_override_wins_over_the_user_home() {
    let tmp = tempfile::tempdir().unwrap();
    let elsewhere = tmp.path().join("elsewhere");
    let _home = EnvVarGuard::set("HOME", tmp.path());
    let _override = EnvVarGuard::set(ASSISTANTS_DIR_ENV, &elsewhere);
    assert_eq!(assistants_root().unwrap(), elsewhere);
}

#[test]
fn ensure_creates_the_whole_layout() {
    let (_tmp, home) = temp_home();
    let created = home.ensure().expect("ensure");
    assert!(home.exists());
    for path in [home.agents_dir(), home.okg_dir(), home.attachments_dir()] {
        assert!(path.is_dir(), "{} should be a directory", path.display());
    }
    for path in [home.instructions_path(), home.config_path()] {
        assert!(path.is_file(), "{} should be a file", path.display());
    }
    assert_eq!(created.paths.len(), 6, "created: {:?}", created.paths);

    // The seeded config parses back into the shape health checks against, and
    // carries the instance id.
    let raw = std::fs::read_to_string(home.config_path()).unwrap();
    let cfg: AssistantHomeConfig = toml::from_str(&raw).expect("seeded config parses");
    assert_eq!(cfg.id, "izzie");
}

#[test]
fn ensure_is_idempotent() {
    let (_tmp, home) = temp_home();
    assert!(!home.ensure().unwrap().is_empty());
    let second = home.ensure().expect("second ensure");
    assert!(second.is_empty(), "created again: {:?}", second.paths);
}

/// Why: #4325 makes external modification EXPECTED. A `ensure` that restored
/// its own seed over a user's edit would be the intolerance the ticket rules
/// out — and would silently destroy work.
#[test]
fn ensure_never_overwrites_user_edits() {
    let (_tmp, home) = temp_home();
    home.ensure().unwrap();
    std::fs::write(home.instructions_path(), "MY OWN INSTRUCTIONS\n").unwrap();
    std::fs::write(home.config_path(), "id = \"izzie\"\nmine = true\n").unwrap();
    std::fs::write(home.okg_dir().join("note.md"), "kept").unwrap();

    home.ensure().expect("re-ensure");

    assert_eq!(
        std::fs::read_to_string(home.instructions_path()).unwrap(),
        "MY OWN INSTRUCTIONS\n"
    );
    assert!(
        std::fs::read_to_string(home.config_path())
            .unwrap()
            .contains("mine = true")
    );
    assert_eq!(
        std::fs::read_to_string(home.okg_dir().join("note.md")).unwrap(),
        "kept"
    );
}

/// Why: a user who deletes just one entry must get it back without the rest
/// being touched — partial repair, not a wholesale regenerate.
#[test]
fn ensure_restores_only_what_is_missing() {
    let (_tmp, home) = temp_home();
    home.ensure().unwrap();
    std::fs::write(home.instructions_path(), "kept\n").unwrap();
    std::fs::remove_dir_all(home.attachments_dir()).unwrap();

    let created = home.ensure().expect("re-ensure");

    assert_eq!(created.paths, vec![home.attachments_dir()]);
    assert_eq!(
        std::fs::read_to_string(home.instructions_path()).unwrap(),
        "kept\n"
    );
}
