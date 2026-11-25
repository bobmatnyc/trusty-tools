//! Owner-only modes for the managed `CLAUDE_CONFIG_DIR` (#11195).
//!
//! Why: the dir holds `.claude.json` (MCP env, which can carry tokens), its
//! `.claude.json.bak`, Claude Code's `backups/.claude.json.backup.<ms>` copies
//! and `settings.json`. Claude Code writes the backups at the umask default and
//! `.claude.json.bak` copies the source's mode, so a one-time chmod of the
//! files does not last. A 0700 dir keeps every file in it unreachable by other
//! users whatever mode a later writer gives the file.
//! What: [`secure_config_dir`] sets the dir and `backups/` to 0700 and the
//! known secret files to 0600, and returns every chmod that did not take.
//! Symlinks are never chmodded through: they could point outside the dir.
//! Test: `tests_config_dir_mode_11195.rs`.

use std::path::{Path, PathBuf};

/// The dir under the config dir where Claude Code keeps `.claude.json` copies.
pub const BACKUPS_DIR: &str = "backups";

/// Secret files directly in the config dir.
const SECRET_FILES: [&str; 3] = [".claude.json", ".claude.json.bak", "settings.json"];

/// Prefix of Claude Code's copies in `backups/`.
const BACKUP_PREFIX: &str = ".claude.json.backup";

/// One chmod that did not take.
#[derive(Debug)]
pub struct ModeFailure {
    /// The path left at its old mode.
    pub path: PathBuf,
    /// Why.
    pub reason: String,
}

/// Tighten the managed config dir, `backups/` and the secret files.
///
/// Why: see the module doc. A failure must reach the caller so launch can log
/// it; the `claude_json_secrets` doctor row still sees the loose mode.
/// What: on unix, 0700 on `dir` and on `dir/backups` (if a real dir), 0600 on
/// each existing `SECRET_FILES` entry and each `backups/.claude.json.backup*`
/// regular file. An absent file is skipped; a symlink is refused. Every step
/// runs; failures are collected. On other platforms it does nothing.
/// Test: `provisioning_leaves_config_dir_backups_and_secret_files_owner_only`,
/// `a_failed_chmod_is_reported_with_its_path`.
pub fn secure_config_dir(dir: &Path) -> Vec<ModeFailure> {
    let mut failures = Vec::new();
    #[cfg(unix)]
    {
        set_mode(dir, 0o700, true, &mut failures);
        let backups = dir.join(BACKUPS_DIR);
        if std::fs::symlink_metadata(&backups).is_ok() {
            set_mode(&backups, 0o700, true, &mut failures);
        }
        for name in SECRET_FILES {
            set_mode(&dir.join(name), 0o600, false, &mut failures);
        }
        if let Ok(entries) = std::fs::read_dir(&backups) {
            for entry in entries.filter_map(Result::ok) {
                if entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(BACKUP_PREFIX)
                {
                    set_mode(&entry.path(), 0o600, false, &mut failures);
                }
            }
        }
    }
    #[cfg(not(unix))]
    let _ = dir;
    failures
}

/// chmod `path` to `mode` unless it is absent; refuse a symlink or a wrong type.
#[cfg(unix)]
fn set_mode(path: &Path, mode: u32, want_dir: bool, failures: &mut Vec<ModeFailure>) {
    use std::os::unix::fs::PermissionsExt;
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => return fail(failures, path, format!("stat failed: {e}")),
    };
    let ft = meta.file_type();
    if ft.is_symlink() {
        return fail(failures, path, "is a symlink; not chmodded through".into());
    }
    if want_dir != ft.is_dir() {
        return fail(failures, path, "unexpected file type".into());
    }
    if meta.permissions().mode() & 0o777 == mode {
        return;
    }
    if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)) {
        fail(failures, path, format!("chmod {mode:o} failed: {e}"));
    }
}

/// Record one failure.
#[cfg(unix)]
fn fail(failures: &mut Vec<ModeFailure>, path: &Path, reason: String) {
    failures.push(ModeFailure {
        path: path.to_path_buf(),
        reason,
    });
}
