//! Where the build-lease store lives (#8261 round 3).
//!
//! Why: the lease caps builds machine-wide only if every `tm build-lease` on
//! the machine locks the SAME files. Round 2 fell back to
//! `$TMPDIR/trusty-mpm-build-slots-<uid>` when `~/.trusty-mpm` was unusable,
//! and `$TMPDIR` differs between sessions, so two sessions could each hold
//! "slot 0" of their own store at once.
//!
//! What: [`SlotDir::resolve`] uses the canonical `~/.trusty-mpm/build-slots`.
//! When that is unusable it uses ONE fixed per-uid path,
//! `/tmp/trusty-mpm-build-slots-<uid>`, never `$TMPDIR` and never `./`. Either
//! store must be a directory owned by this uid with no group or other write
//! bit; a directory this uid owns is narrowed to `0700`, one it does not own is
//! refused. When neither is usable the error names both paths, both OS errors
//! and the repair, and `tm build-lease` refuses the build (exit 75).
//!
//! Debug builds only: `TRUSTY_MPM_TEST_BUILD_SLOT_FALLBACK` replaces the fixed
//! fallback path, so integration tests can break or isolate it without
//! touching the machine's real `/tmp` store. A release build (`cargo install`)
//! compiles the override out.
//! Test: the `#[cfg(test)]` suite below; `tests/tm_build_lease.rs`.

use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use super::slots::{SLOT_DIR_NAME, SlotDir, current_uid};

/// Why no build-lease store is usable.
///
/// Test: `a_store_error_names_both_paths_and_the_repair`.
#[derive(Debug, thiserror::Error)]
#[error(
    "no build-lease store is usable: {canonical}: {canonical_error}; fallback {fallback}: \
     {fallback_error}. Repair: {}",
    self.repair()
)]
#[non_exhaustive]
pub struct StoreError {
    /// The canonical store path.
    pub canonical: PathBuf,
    /// Why it could not be used.
    pub canonical_error: String,
    /// The fixed per-uid fallback path.
    pub fallback: PathBuf,
    /// Why it could not be used.
    pub fallback_error: String,
}

impl StoreError {
    /// How to repair the canonical store.
    #[must_use]
    pub fn repair(&self) -> String {
        format!(
            "make {0} a directory you own with no group or other write permission \
             (`mkdir -p {0} && chmod 700 {0}`), moving aside any file in its place",
            self.canonical.display()
        )
    }
}

/// `<home>/.trusty-mpm/build-slots`.
#[must_use]
pub fn canonical_path(home: &Path) -> PathBuf {
    home.join(".trusty-mpm").join(SLOT_DIR_NAME)
}

/// The fixed per-uid fallback: `/tmp/trusty-mpm-build-slots-<uid>`.
///
/// What: never derived from `$TMPDIR`. Debug builds honour the test override
/// named in the module doc.
/// Test: `the_fallback_path_ignores_tmpdir`.
#[must_use]
pub fn fallback_path() -> PathBuf {
    #[cfg(debug_assertions)]
    if let Some(dir) = std::env::var_os("TRUSTY_MPM_TEST_BUILD_SLOT_FALLBACK") {
        return PathBuf::from(dir);
    }
    PathBuf::from("/tmp").join(format!("trusty-mpm-{SLOT_DIR_NAME}-{}", current_uid()))
}

impl SlotDir {
    /// The machine-wide store: canonical, else the fixed per-uid fallback.
    ///
    /// # Errors
    ///
    /// [`StoreError`] when neither is usable. `home` of `None` (no home
    /// directory) makes the canonical path unusable, never `./`.
    ///
    /// Test: `a_broken_home_falls_back_to_the_fixed_per_uid_store`,
    /// `a_store_error_names_both_paths_and_the_repair`.
    pub fn resolve(home: Option<&Path>) -> Result<Self, StoreError> {
        Self::resolve_in(home, &fallback_path())
    }

    /// [`Self::resolve`] with the fallback path given.
    ///
    /// # Errors
    ///
    /// As [`Self::resolve`].
    ///
    /// Test: as [`Self::resolve`].
    pub fn resolve_in(home: Option<&Path>, fallback: &Path) -> Result<Self, StoreError> {
        let canonical = home.map_or_else(
            || PathBuf::from("~/.trusty-mpm").join(SLOT_DIR_NAME),
            canonical_path,
        );
        let canonical_error = match home {
            None => "no home directory".to_string(),
            Some(_) => match open_private_dir(&canonical, true) {
                Ok(dir) => return Ok(dir),
                Err(err) => err,
            },
        };
        match open_private_dir(fallback, false) {
            Ok(dir) => Ok(dir.into_fallback(format!("{}: {canonical_error}", canonical.display()))),
            Err(fallback_error) => Err(StoreError {
                canonical,
                canonical_error,
                fallback: fallback.to_path_buf(),
                fallback_error,
            }),
        }
    }
}

/// Create (mode `0700`) or open `path`, then check it is private to this uid.
///
/// What: `recursive` creates missing parents (the canonical path); the
/// fallback's parent `/tmp` must exist, and a symlink there is refused.
fn open_private_dir(path: &Path, recursive: bool) -> Result<SlotDir, String> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(recursive).mode(0o700);
    match builder.create(path) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(err) => return Err(err.to_string()),
    }
    let meta = if recursive {
        std::fs::metadata(path)
    } else {
        std::fs::symlink_metadata(path)
    }
    .map_err(|err| err.to_string())?;
    if !meta.is_dir() {
        return Err("not a directory".to_string());
    }
    if meta.uid() != current_uid() {
        return Err(format!(
            "owned by uid {}, not this user's uid {}",
            meta.uid(),
            current_uid()
        ));
    }
    if meta.mode() & 0o022 != 0 {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).map_err(|err| {
            format!(
                "mode {:o} is group- or other-writable and could not be narrowed: {err}",
                meta.mode() & 0o777
            )
        })?;
    }
    SlotDir::at(path).map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_broken_home_falls_back_to_the_fixed_per_uid_store() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join(".trusty-mpm"), "x").expect("a file, not a dir");
        let fallback = tmp.path().join("fixed");
        let slots = SlotDir::resolve_in(Some(tmp.path()), &fallback).expect("fallback");
        assert_eq!(slots.path(), fallback);
        let why = slots.fallback_reason().expect("marked as the fallback");
        assert!(why.contains(".trusty-mpm/build-slots"), "{why}");
        let good = tempfile::tempdir().expect("tempdir");
        let slots = SlotDir::resolve_in(Some(good.path()), &fallback).expect("canonical");
        assert_eq!(slots.path(), canonical_path(good.path()));
        assert!(slots.fallback_reason().is_none());
    }

    #[test]
    fn a_store_error_names_both_paths_and_the_repair() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join(".trusty-mpm"), "x").expect("file");
        std::fs::write(tmp.path().join("blocker"), "x").expect("file");
        let err = SlotDir::resolve_in(Some(tmp.path()), &tmp.path().join("blocker/sub"))
            .expect_err("neither is usable");
        let text = err.to_string();
        for needle in [
            ".trusty-mpm/build-slots",
            "blocker/sub",
            "Repair:",
            "chmod 700",
        ] {
            assert!(text.contains(needle), "{needle}: {text}");
        }
        assert!(
            SlotDir::resolve_in(None, &tmp.path().join("blocker/sub")).is_err(),
            "no home never means ./"
        );
    }

    #[test]
    fn a_writable_by_others_store_is_narrowed() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("fallback");
        std::fs::create_dir(&dir).expect("mkdir");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).expect("chmod");
        std::fs::write(tmp.path().join(".trusty-mpm"), "x").expect("file");
        SlotDir::resolve_in(Some(tmp.path()), &dir).expect("owned by us, so narrowed");
        let mode = std::fs::metadata(&dir).expect("stat").permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "{mode:o}");
    }

    #[test]
    fn the_fallback_path_ignores_tmpdir() {
        if std::env::var_os("TRUSTY_MPM_TEST_BUILD_SLOT_FALLBACK").is_some() {
            return;
        }
        let path = fallback_path();
        assert!(path.starts_with("/tmp"), "{path:?}");
        assert!(
            path.ends_with(format!("trusty-mpm-build-slots-{}", current_uid())),
            "{path:?}"
        );
    }
}
