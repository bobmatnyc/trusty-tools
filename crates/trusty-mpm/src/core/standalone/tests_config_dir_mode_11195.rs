//! Launch-time mode coverage for #11195: provisioning the managed
//! `CLAUDE_CONFIG_DIR` leaves the dir, its `backups/` dir and the secret files
//! in them owner-only.
//!
//! Why: Claude Code (run with `CLAUDE_CONFIG_DIR` pointed at the dir) rewrites
//! `.claude.json` and writes `backups/.claude.json.backup.<ms>` at the umask
//! default, and `.claude.json.bak` copies the source file's mode. A one-time
//! `tm doctor --fix` chmod did not survive the next session. Making the dirs
//! 0700 on every launch keeps those files unreachable whatever mode a later
//! writer gives them.
//! What: one arm provisions a 0755 dir holding 0644 secret files through the
//! production entry point and asserts the modes after; one arm forces a chmod
//! failure and asserts it is reported with the path, not swallowed.
//! Test: this module IS the test suite.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::test_support::hermetic_temp_dir;

/// See `tests_malformed_settings_7789::TEST_EXE`.
const TEST_EXE: &str = "/usr/local/bin/tm";

/// Permission bits of `path`.
fn mode(path: &Path) -> u32 {
    std::fs::symlink_metadata(path)
        .expect("stat")
        .permissions()
        .mode()
        & 0o777
}

/// Write `path` and set it to `mode`.
fn write_at(path: &Path, body: &str, mode: u32) {
    std::fs::write(path, body).expect("write");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
}

/// Set a directory's mode.
fn chmod_dir(path: &Path, mode: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod dir");
}

/// Provisioning tightens the dirs to 0700 and the secret files to 0600.
#[test]
fn provisioning_leaves_config_dir_backups_and_secret_files_owner_only() {
    let tmp = hermetic_temp_dir();
    let managed_root = tmp.path().join("managed");
    let cfg = managed_root.join("claude-config");
    let backups = cfg.join("backups");
    std::fs::create_dir_all(&backups).expect("create dirs");
    chmod_dir(&cfg, 0o755);
    chmod_dir(&backups, 0o755);
    write_at(&cfg.join(".claude.json"), "{}", 0o644);
    write_at(&cfg.join(".claude.json.bak"), "{}", 0o644);
    write_at(&cfg.join("settings.json"), "{}", 0o644);
    let backup_copy = backups.join(".claude.json.backup.1763800000000");
    write_at(&backup_copy, "{}", 0o644);

    super::global_config::ensure_global_config_dir_with_exe(
        &managed_root,
        &cfg,
        Some(Path::new(TEST_EXE)),
    )
    .expect("provisioning succeeds");

    assert_eq!(mode(&cfg), 0o700, "claude-config/");
    assert_eq!(mode(&backups), 0o700, "claude-config/backups/");
    for f in [
        cfg.join(".claude.json"),
        cfg.join(".claude.json.bak"),
        cfg.join("settings.json"),
        backup_copy,
    ] {
        assert_eq!(mode(&f), 0o600, "{}", f.display());
    }
}

/// A chmod that fails is returned with its path, never dropped.
///
/// The failure is forced structurally, whatever the test's uid: `settings.json`
/// is a symlink, which the pass refuses to chmod through.
#[test]
fn a_failed_chmod_is_reported_with_its_path() {
    let tmp = hermetic_temp_dir();
    let cfg = tmp.path().join("claude-config");
    std::fs::create_dir_all(&cfg).expect("create dir");
    write_at(&cfg.join(".claude.json"), "{}", 0o644);
    let target = cfg.join(".claude.json");
    // A symlinked secret file is never chmodded through (it could point out of
    // the dir); it is reported instead.
    let linked = cfg.join("settings.json");
    std::os::unix::fs::symlink(&target, &linked).expect("symlink");

    let failures = super::config_dir_mode::secure_config_dir(&cfg);

    assert!(
        failures.iter().any(|f| f.path == linked),
        "the refused path must be reported: {failures:?}"
    );
    assert_eq!(mode(&cfg), 0o700, "the rest still runs");
    assert_eq!(mode(&target), 0o600, "the real file is tightened");
}
