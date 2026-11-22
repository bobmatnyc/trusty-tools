//! Data stays in the pre-rename folders (#11632, owner ruling Dcm).
//!
//! Why: the owner ruled that the rename moves no data. Existing assistant
//! homes live in `~/trusty-agents/<id>/` and config in `~/.trusty-agents/`;
//! a new default or a move would strand them or rewrite them.
//! What: under the temporary `$HOME` `common` installs, checks the
//! assistants-root override order and the defaults in-process, then runs the
//! real `ta` against seeded old folders and checks it reads and writes them
//! and creates nothing under the new crate name.
//! Test: this file IS the test.

mod common;

use std::path::{Path, PathBuf};
use std::process::Stdio;

use serial_test::serial;
use trusty_assistants::assistants::{
    ASSISTANTS_DIR_ENV, LEGACY_ASSISTANTS_DIR_ENV, app_dir, assistants_root,
};

/// The two names a move or a new default would create.
fn assert_no_new_folders(home: &Path) {
    for name in ["trusty-assistants", ".trusty-assistants"] {
        assert!(
            std::fs::symlink_metadata(home.join(name)).is_err(),
            "{name} must not exist under {}",
            home.display()
        );
    }
}

/// Why: `TA_ASSISTANTS_DIR` wins, `TAGENT_ASSISTANTS_DIR` still works, and
/// with neither set the homes root and the app directory are the old names.
/// One test, because the variables are process-global.
/// Test: itself.
#[test]
#[serial]
fn assistants_root_env_order_and_default() {
    let home = PathBuf::from(std::env::var_os("HOME").expect("sandbox HOME"));
    assert!(home.starts_with(std::env::temp_dir()), "{}", home.display());
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let (new, old) = (tmp.path().join("new"), tmp.path().join("old"));

    // SAFETY: `#[serial]`; every test in this binary that reads the
    // environment is serialized with this one.
    unsafe {
        std::env::remove_var(ASSISTANTS_DIR_ENV);
        std::env::remove_var(LEGACY_ASSISTANTS_DIR_ENV);
    }
    assert_eq!(
        assistants_root().expect("default"),
        home.join("trusty-agents")
    );
    assert_eq!(app_dir(&home), home.join(".trusty-agents"));

    unsafe { std::env::set_var(LEGACY_ASSISTANTS_DIR_ENV, &old) };
    assert_eq!(assistants_root().expect("old variable"), old);

    unsafe { std::env::set_var(ASSISTANTS_DIR_ENV, &new) };
    assert_eq!(assistants_root().expect("new variable"), new);

    unsafe {
        std::env::remove_var(ASSISTANTS_DIR_ENV);
        std::env::remove_var(LEGACY_ASSISTANTS_DIR_ENV);
    }
    assert_no_new_folders(&home);
}

/// Why: the shipped binary must read the old folders where they are, write
/// back into them, and neither move them nor create the new names.
/// Test: itself.
#[test]
#[serial]
fn ta_keeps_the_old_folders_and_creates_no_new_ones() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let home = tmp.path();
    std::fs::create_dir_all(home.join(".trusty-agents")).expect("mkdir");
    std::fs::write(
        home.join(".trusty-agents/user.toml"),
        "name = \"Ada Pilot\"\ncreated_at = \"2026-01-01T00:00:00Z\"\n",
    )
    .expect("seed profile");
    for id in ["assistant", "cto-assistant", "izzie", "writing-assistant"] {
        std::fs::create_dir_all(home.join("trusty-agents").join(id)).expect("mkdir home");
        std::fs::write(
            home.join("trusty-agents").join(id).join("instructions.md"),
            id,
        )
        .expect("seed instructions");
    }

    let show = common::ta(&tmp)
        .args(["config", "profile", "show"])
        .env_remove("TAGENT_USER_NAME")
        .output()
        .expect("spawn ta config profile show");
    let stdout = String::from_utf8_lossy(&show.stdout);
    assert!(
        show.status.success(),
        "{}",
        String::from_utf8_lossy(&show.stderr)
    );
    assert!(stdout.contains("Ada Pilot"), "{stdout}");
    let saved_at = home.join(".trusty-agents").join("user.toml");
    assert!(stdout.contains(&saved_at.display().to_string()), "{stdout}");

    let set = common::ta(&tmp)
        .args(["config", "profile", "set", "--name", "Bea Pilot"])
        .output()
        .expect("spawn ta config profile set");
    assert!(
        set.status.success(),
        "{}",
        String::from_utf8_lossy(&set.stderr)
    );
    let written = std::fs::read_to_string(&saved_at).expect("profile still in the old folder");
    assert!(written.contains("Bea Pilot"), "{written}");

    let serve = common::ta(&tmp)
        .arg("mcp-serve")
        .stdin(Stdio::null())
        .output()
        .expect("spawn ta mcp-serve");
    assert!(
        serve.status.success(),
        "{}",
        String::from_utf8_lossy(&serve.stderr)
    );

    for dir in [home.join(".trusty-agents"), home.join("trusty-agents")] {
        let meta = std::fs::symlink_metadata(&dir).expect("old folder still there");
        assert!(
            meta.is_dir(),
            "{} must stay a real directory",
            dir.display()
        );
    }
    for id in ["assistant", "cto-assistant", "izzie", "writing-assistant"] {
        let body =
            std::fs::read_to_string(home.join("trusty-agents").join(id).join("instructions.md"))
                .expect("home untouched");
        assert_eq!(body, id);
    }
    assert_no_new_folders(home);
}
