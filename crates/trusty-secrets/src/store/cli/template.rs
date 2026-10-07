//! [`TemplateFile`] and [`sweep_stale_templates`]: a value-bearing file for
//! a CLI that reads its input only from a path.
//!
//! Why: #7519, owner ruling 2026-10-07 — `op item edit <id> --template
//! <file>` takes its JSON template from a file, not stdin. The file holds a
//! value, so it is owner-only, lives under [`TMP_SUBDIR`], exists for one
//! call, and a crashed process's leftover is removed at the next start.
//! What: [`TemplateFile::create`] makes a `tpl.<pid>.<nanos>.<seq>/`
//! directory (0700) under the tmp root and `template.json` (0600,
//! `create_new`, no-follow) inside, and writes the bytes; drop removes the
//! file, then the directory, on success, error and panic paths alike.
//! [`sweep_stale_templates`] removes guard directories whose creating
//! process is gone, without following a symlink.
//! Test: `template_file_is_0600_in_a_0700_dir_and_removed_on_drop`,
//! `template_file_is_removed_when_its_scope_panics`,
//! `template_sweep_removes_stale_dirs_and_leaves_the_rest`.

use std::fs::{self, DirBuilder, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::api::{SecretValue, SecretsError};
use crate::store::file::{self, Kind};
use crate::store::platform;

/// The tmp root, relative to `$HOME`.
pub const TMP_SUBDIR: &str = ".trusty-tools/trusty-secrets/tmp";

/// Every guard directory's name starts with this; the sweep touches no other.
const DIR_PREFIX: &str = "tpl.";

/// The template's file name inside its guard directory.
const FILE_NAME: &str = "template.json";

/// Makes two guards created in one nanosecond distinct.
static SEQ: AtomicU64 = AtomicU64::new(0);

/// `~/.trusty-tools/trusty-secrets/tmp`.
pub fn default_tmp_root() -> Result<PathBuf, SecretsError> {
    Ok(platform::home_dir()?.join(TMP_SUBDIR))
}

/// An owner-only file holding one template, removed on drop.
///
/// Why: see the module docs.
/// What: owns a private directory and the one file in it. `Debug` shows
/// the paths only.
/// Test: `template_file_is_0600_in_a_0700_dir_and_removed_on_drop`,
/// `template_file_is_removed_when_its_scope_panics`.
#[derive(Debug)]
pub struct TemplateFile {
    dir: PathBuf,
    path: PathBuf,
}

impl TemplateFile {
    /// Write `contents` to a fresh template file under `root`.
    ///
    /// What: creates `root` (0700, with parents) and refuses it if it is a
    /// symlink, wider than 0700 or another user's
    /// ([`SecretsError::StorageRefused`]). Then creates a fresh guard
    /// directory (0700, never an existing one) and the file in it (0600,
    /// `create_new`, `O_NOFOLLOW`), writes and syncs. A failure after the
    /// directory exists removes what was made.
    /// Test: `template_file_is_0600_in_a_0700_dir_and_removed_on_drop`.
    pub fn create(root: &Path, contents: &SecretValue) -> Result<Self, SecretsError> {
        file::create_dir(root, true)?;
        let dir = root.join(unique_name());
        DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .map_err(io_err(&dir))?;
        // #7519: from here the guard's drop removes whatever was created.
        let guard = Self {
            path: dir.join(FILE_NAME),
            dir,
        };
        let written = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&guard.path)
            .and_then(|mut f| {
                f.write_all(contents.expose().as_bytes())?;
                f.sync_all()
            });
        written.map_err(io_err(&guard.path))?;
        Ok(guard)
    }

    /// The file to hand the CLI.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TemplateFile {
    fn drop(&mut self) {
        // Errors are ignored: a leftover is the startup sweep's to remove.
        let _ = fs::remove_file(&self.path);
        let _ = fs::remove_dir(&self.dir);
    }
}

/// Remove the guard directories under `root` whose creating process is gone.
///
/// Why: a crash or `SIGKILL` skips [`TemplateFile`]'s drop and leaves a
/// value on disk; a backend calls this once at startup.
/// What: an absent `root` is `Ok(0)`; a `root` that is a symlink, wider
/// than 0700 or another user's is [`SecretsError::StorageRefused`]. Only
/// real directories (never a symlink) named `tpl.<pid>.…` whose pid is not
/// live, as the file backend judges a temp file's writer, are touched:
/// their `template.json` entry is unlinked, never followed, then the
/// directory is removed. One holding anything else is left alone. Returns
/// how many directories were removed.
/// Test: `template_sweep_removes_stale_dirs_and_leaves_the_rest`.
pub fn sweep_stale_templates(root: &Path) -> Result<usize, SecretsError> {
    let meta = match fs::symlink_metadata(root) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(source) => return Err(io_err(root)(source)),
    };
    file::judge(root, &meta, Kind::Dir)?;
    let mut removed = 0;
    for entry in fs::read_dir(root).map_err(io_err(root))? {
        let entry = entry.map_err(io_err(root))?;
        let name = entry.file_name();
        let Some(rest) = name.to_str().and_then(|n| n.strip_prefix(DIR_PREFIX)) else {
            continue;
        };
        // #7519: `DirEntry::file_type` does not follow a symlink.
        if !entry.file_type().map_err(io_err(root))?.is_dir() || file::writer_alive(rest) {
            continue;
        }
        let dir = entry.path();
        let template = dir.join(FILE_NAME);
        match fs::remove_file(&template) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => return Err(io_err(&template)(source)),
        }
        match fs::remove_dir(&dir) {
            Ok(()) => removed += 1,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
                ) => {}
            Err(source) => return Err(io_err(&dir)(source)),
        }
    }
    Ok(removed)
}

/// `tpl.<pid>.<nanos>.<seq>`: the pid lets the sweep judge liveness.
fn unique_name() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    format!("{DIR_PREFIX}{}.{nanos}.{seq}", std::process::id())
}

fn io_err(path: &Path) -> impl FnOnce(std::io::Error) -> SecretsError + '_ {
    move |source| SecretsError::Io {
        path: path.to_path_buf(),
        source,
    }
}
