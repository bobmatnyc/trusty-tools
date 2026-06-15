//! Project-root detection, palace-slug derivation, and `.trusty-tools/` pin
//! file management (issue #88 + Phase 1 of the `.trusty-tools/` convention).
//!
//! Why: unbounded palace creation leads to orphaned namespaces that no longer
//! correspond to any project on disk. Anchoring palace names to a stable,
//! filesystem-derived slug ensures each project gets exactly one palace and
//! makes "which palace am I in?" predictable from the working directory alone.
//! The `personal` palace is the single sanctioned exception for non-project
//! contexts (global notes, one-off sessions).
//!
//! Sub-modules:
//!   - `detection`: `find_project_root`, `PROJECT_MARKERS`, `TRUSTY_TOOLS_DIR`,
//!     `PERSONAL_PALACE`, `is_unsafe_pin_location`.
//!   - `pin_file`: `ProjectPin`, `PIN_SCHEMA_VERSION`, `PIN_FILE_REL`,
//!     `read_project_pin`, `write_project_pin`, `project_slug_at`,
//!     `project_slug_at_readonly`, `project_slug`, `project_slug_from_basename`.
//!   - `validation`: `validate_palace_name`.
//!
//! Test: `project_slug_finds_git_root`, `project_slug_returns_none_without_markers`,
//! `project_slug_uses_first_ancestor_marker`,
//! `project_slug_personal_always_allowed`,
//! `pin_file_read_when_present`, `absent_pin_writes_computed_slug`,
//! `renamed_dir_with_pin_resolves_to_original_slug`,
//! `trusty_tools_dir_is_project_marker`,
//! `lazy_write_non_fatal_on_readonly_dir`.

mod detection;
mod pin_file;
mod validation;

pub use detection::{find_project_root, PERSONAL_PALACE, PROJECT_MARKERS, TRUSTY_TOOLS_DIR};
pub use pin_file::{
    pinned_slug_at, project_slug, project_slug_at, project_slug_at_readonly,
    project_slug_from_basename, read_project_pin, write_project_pin, ProjectPin, PIN_FILE_REL,
    PIN_SCHEMA_VERSION,
};
pub use validation::validate_palace_name;

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // -----------------------------------------------------------------------
    // find_project_root
    // -----------------------------------------------------------------------

    /// Why: the primary use-case — a nested directory inside a git repo must
    /// resolve to the repo root, not just the immediate parent.
    /// What: create a temp dir with a `.git` subdir, nest a subdirectory
    /// inside it, and assert that `find_project_root` from the subdirectory
    /// returns the outer root (the one with `.git`).
    /// Test: itself.
    #[test]
    fn project_slug_finds_git_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        // Create a .git marker at the root level.
        fs::create_dir_all(root.join(".git")).unwrap();
        // Create a nested subdirectory.
        let nested = root.join("crates").join("foo");
        fs::create_dir_all(&nested).unwrap();

        let found = find_project_root(&nested);
        assert!(found.is_some(), "should find project root");
        // Canonicalize both sides so macOS /var vs /private/var symlinks
        // do not cause false mismatches.
        let found_canonical = fs::canonicalize(found.unwrap()).unwrap();
        let root_canonical = fs::canonicalize(&root).unwrap();
        assert_eq!(found_canonical, root_canonical);
    }

    /// Why: when the CWD is not inside any project, `find_project_root` must
    /// return `None` so the caller can fall through to the `personal` palace.
    /// What: create a temp dir with *no* marker files and assert the result
    /// is `None`.
    /// Test: itself.
    #[test]
    fn project_slug_returns_none_without_markers() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // Bare directory — no .git, Cargo.toml, etc.
        let found = find_project_root(tmp.path());
        assert!(
            found.is_none(),
            "bare tempdir should not resolve to a project root"
        );
    }

    /// Why: `Cargo.toml` is also a valid project marker; not every project
    /// uses git.
    /// What: create a temp dir with a `Cargo.toml` file and assert it is
    /// detected as the project root from a subdirectory.
    /// Test: itself.
    #[test]
    fn project_slug_uses_first_ancestor_marker() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        fs::write(root.join("Cargo.toml"), "[package]").unwrap();
        let sub = root.join("src");
        fs::create_dir_all(&sub).unwrap();

        let found = find_project_root(&sub);
        assert!(found.is_some());
        // Canonicalize both sides so macOS /var vs /private/var symlinks
        // do not cause false mismatches.
        let found_canonical = fs::canonicalize(found.unwrap()).unwrap();
        let root_canonical = fs::canonicalize(&root).unwrap();
        assert_eq!(found_canonical, root_canonical);
    }

    // -----------------------------------------------------------------------
    // project_slug_at
    // -----------------------------------------------------------------------

    /// Why: the slug must be the slugified basename of the project root, not
    /// the subdirectory we started from.
    /// What: create a root named `my-project` with a `.git` marker; start
    /// from a nested subdirectory; assert the slug is `my-project`.
    /// Test: itself.
    #[test]
    fn project_slug_at_returns_root_basename_slug() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("my-project");
        fs::create_dir_all(root.join(".git")).unwrap();
        let src = root.join("src");
        fs::create_dir_all(&src).unwrap();

        let slug = project_slug_at(&src).expect("should return slug");
        assert_eq!(slug, "my-project");
    }

    /// Why: uppercase and underscores must be normalised by the slug derivation
    /// so that `My_Project` and `my-project` resolve to the same palace.
    /// What: create a root named `My_Project`; assert the derived slug is
    /// `my-project`.
    /// Test: itself.
    #[test]
    fn project_slug_at_normalises_case_and_underscores() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("My_Project");
        fs::create_dir_all(root.join(".git")).unwrap();

        let slug = project_slug_at(&root).expect("should return slug");
        assert_eq!(slug, "my-project");
    }

    /// Why: when no project root is found, `project_slug_at` must return
    /// `None` so the caller knows to use `personal`.
    /// Test: itself.
    #[test]
    fn project_slug_at_returns_none_without_markers() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(project_slug_at(tmp.path()).is_none());
    }

    // -----------------------------------------------------------------------
    // validate_palace_name
    // -----------------------------------------------------------------------

    /// Why: `personal` is the sanctioned escape hatch; it must always be
    /// accepted regardless of whether a project root is found.
    /// What: run `validate_palace_name("personal", …)` from a plain temp
    /// dir (no project markers); assert `Ok(())`.
    /// Test: itself.
    #[test]
    fn validate_palace_name_accepts_personal() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let result = validate_palace_name(PERSONAL_PALACE, tmp.path());
        assert!(
            result.is_ok(),
            "personal must always be accepted; got {result:?}"
        );
    }

    /// Why: when the name exactly matches the derived slug the creation must
    /// succeed.
    /// What: create a project root named `cool-app`; assert that
    /// `validate_palace_name("cool-app", subdir)` returns `Ok(())`.
    /// Test: itself.
    #[test]
    fn validate_palace_name_accepts_matching_slug() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("cool-app");
        fs::create_dir_all(root.join(".git")).unwrap();
        let sub = root.join("src");
        fs::create_dir_all(&sub).unwrap();

        let result = validate_palace_name("cool-app", &sub);
        assert!(result.is_ok(), "matching slug must be accepted: {result:?}");
    }

    /// Why: a mismatched name must be rejected with an actionable error that
    /// tells the user which slug is expected.
    /// What: create a project root named `cool-app`; assert that
    /// `validate_palace_name("wrong-name", subdir)` returns `Err` and the
    /// error message mentions `cool-app`.
    /// Test: itself.
    #[test]
    fn validate_palace_name_rejects_mismatch() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("cool-app");
        fs::create_dir_all(root.join(".git")).unwrap();
        let sub = root.join("src");
        fs::create_dir_all(&sub).unwrap();

        let result = validate_palace_name("wrong-name", &sub);
        assert!(result.is_err(), "mismatched name must be rejected");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("cool-app"),
            "error must mention the expected slug; got: {msg}"
        );
    }

    /// Why: outside a project directory, only `personal` is allowed; any
    /// other name must be rejected.
    /// What: use a plain tempdir (no markers); assert that any non-`personal`
    /// name returns `Err`.
    /// Test: itself.
    #[test]
    fn validate_palace_name_rejects_non_personal_without_project() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let result = validate_palace_name("my-notes", tmp.path());
        assert!(
            result.is_err(),
            "non-personal name outside a project must be rejected"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("personal"),
            "error must mention 'personal'; got: {msg}"
        );
    }

    // -----------------------------------------------------------------------
    // Pin-file helpers: read_project_pin / write_project_pin
    // -----------------------------------------------------------------------

    /// Why: the round-trip must be lossless — what we write we must be able
    /// to read back with the same slug value.
    /// What: writes a pin, reads it back, asserts all fields match.
    /// Test: itself.
    #[test]
    fn write_and_read_pin_round_trips() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let pin = ProjectPin {
            schema_version: PIN_SCHEMA_VERSION,
            palace: "my-project".to_string(),
            note: None,
        };
        write_project_pin(tmp.path(), &pin).expect("write ok");
        let read_back = read_project_pin(tmp.path())
            .expect("read ok")
            .expect("Some(pin)");
        assert_eq!(read_back, pin);
    }

    /// Why: the `note` field is optional; serialising without it must not emit
    /// a `note: null` line in the YAML (which would confuse minimal parsers).
    /// What: write a pin without `note`, read the raw YAML, assert it does not
    /// contain the word `null`.
    /// Test: itself.
    #[test]
    fn write_pin_omits_null_note() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let pin = ProjectPin {
            schema_version: PIN_SCHEMA_VERSION,
            palace: "alpha".to_string(),
            note: None,
        };
        let path = write_project_pin(tmp.path(), &pin).expect("write ok");
        let raw = std::fs::read_to_string(&path).expect("read raw ok");
        assert!(
            !raw.contains("null"),
            "null note must be omitted; got:\n{raw}"
        );
        assert!(raw.contains("palace: alpha"), "slug must be present");
        assert!(
            raw.contains("schema_version: 1"),
            "schema_version must be present"
        );
    }

    /// Why: `read_project_pin` must return `None` (not an error) when no pin
    /// file has been written yet, so callers can fall through to basename
    /// derivation without unwrapping an error.
    /// Test: itself.
    #[test]
    fn read_project_pin_returns_none_when_absent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let result = read_project_pin(tmp.path()).expect("no error");
        assert!(result.is_none(), "absent pin must yield None");
    }

    // -----------------------------------------------------------------------
    // Phase-1 resolution order in project_slug_at
    // -----------------------------------------------------------------------

    /// Why: when a pin file is present it must override the directory basename,
    /// which is the core goal of Phase 1.
    /// What: create a root named `actual-dir`, write a pin file with
    /// `palace: pinned-slug`, then assert `project_slug_at` from a sub-
    /// directory returns `"pinned-slug"` (not `"actual-dir"`).
    /// Test: itself.
    #[test]
    fn pin_file_read_when_present() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("actual-dir");
        fs::create_dir_all(root.join(".git")).unwrap();
        let pin = ProjectPin {
            schema_version: PIN_SCHEMA_VERSION,
            palace: "pinned-slug".to_string(),
            note: None,
        };
        write_project_pin(&root, &pin).expect("write pin");

        let sub = root.join("src");
        fs::create_dir_all(&sub).unwrap();
        let slug = project_slug_at(&sub).expect("slug");
        assert_eq!(
            slug, "pinned-slug",
            "pin file must override the directory basename"
        );
    }

    /// Why: when no pin file exists, `project_slug_at` must lazily create one
    /// so subsequent calls (or after a rename) use the file instead of the
    /// basename.
    /// What: create a project root with a `.git` marker but no pin file; call
    /// `project_slug_at`; assert the pin file was created with the expected slug.
    /// Test: itself.
    #[test]
    fn absent_pin_writes_computed_slug() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("my-cool-project");
        fs::create_dir_all(root.join(".git")).unwrap();

        // No pin file yet.
        assert!(
            read_project_pin(&root).expect("no err").is_none(),
            "no pin before first call"
        );

        let slug = project_slug_at(&root).expect("slug");
        assert_eq!(slug, "my-cool-project");

        // Pin file must now exist.
        let pin = read_project_pin(&root)
            .expect("no err")
            .expect("pin written");
        assert_eq!(pin.palace, "my-cool-project");
        assert_eq!(pin.schema_version, PIN_SCHEMA_VERSION);
    }

    /// Why: the central use-case for Phase 1 — a project with a pin file
    /// returns the original slug even after the directory is renamed.
    /// What: create `old-name/` with `.git` + a pin file set to
    /// `"original-slug"`; rename the directory to `new-name/`; assert that
    /// `project_slug_at` from inside `new-name/` returns `"original-slug"`.
    /// Test: itself.
    #[test]
    fn renamed_dir_with_pin_resolves_to_original_slug() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let old_root = tmp.path().join("old-name");
        fs::create_dir_all(old_root.join(".git")).unwrap();
        let pin = ProjectPin {
            schema_version: PIN_SCHEMA_VERSION,
            palace: "original-slug".to_string(),
            note: None,
        };
        write_project_pin(&old_root, &pin).expect("write pin");

        // Simulate a directory rename.
        let new_root = tmp.path().join("new-name");
        fs::rename(&old_root, &new_root).expect("rename");

        let sub = new_root.join("src");
        fs::create_dir_all(&sub).unwrap();
        let slug = project_slug_at(&sub).expect("slug after rename");
        assert_eq!(
            slug, "original-slug",
            "pin file must survive the directory rename"
        );
    }

    /// Why: decision D5 — a directory containing only `.trusty-tools/` must be
    /// recognised as a project root so the pin file can be found without any
    /// other ecosystem marker (`.git`, `Cargo.toml`, etc.).
    /// What: create a bare tempdir, add only `.trusty-tools/`, assert that
    /// `find_project_root` identifies it as the root.
    /// Test: itself.
    #[test]
    fn trusty_tools_dir_is_project_marker() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(tmp.path().join(TRUSTY_TOOLS_DIR)).unwrap();
        let found = find_project_root(tmp.path());
        assert!(
            found.is_some(),
            ".trusty-tools must trigger project-root detection"
        );
    }

    // -----------------------------------------------------------------------
    // pinned_slug_at (issue #1217)
    // -----------------------------------------------------------------------

    /// Why: `pinned_slug_at` must return the pinned slug when a pin file exists
    /// — this is the backward-compat anchor that keeps already-pinned projects
    /// from being re-derived by the new git/dir scheme.
    /// What: write a pin for `original-slug` under a `.git` root, call from a
    /// subdirectory, assert the pinned slug is returned.
    /// Test: itself.
    #[test]
    fn pinned_slug_at_returns_pin_when_present() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("renamed-dir");
        fs::create_dir_all(root.join(".git")).unwrap();
        let pin = ProjectPin {
            schema_version: PIN_SCHEMA_VERSION,
            palace: "original-slug".to_string(),
            note: None,
        };
        write_project_pin(&root, &pin).expect("write pin");
        let sub = root.join("src");
        fs::create_dir_all(&sub).unwrap();
        assert_eq!(pinned_slug_at(&sub).as_deref(), Some("original-slug"));
    }

    /// Why: when no pin file exists `pinned_slug_at` must return `None` (NOT the
    /// basename) so the caller falls through to identity derivation. This is the
    /// behavioural difference from `project_slug_at_readonly`.
    /// What: a `.git` root with no pin file; assert `None` and that no pin file
    /// was created as a side-effect.
    /// Test: itself.
    #[test]
    fn pinned_slug_at_returns_none_without_pin() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("my-repo");
        fs::create_dir_all(root.join(".git")).unwrap();
        assert!(
            pinned_slug_at(&root).is_none(),
            "no pin file must yield None, not the basename"
        );
        // Must not write a pin file.
        assert!(read_project_pin(&root).expect("no err").is_none());
    }

    // -----------------------------------------------------------------------
    // project_slug_at_readonly
    // -----------------------------------------------------------------------

    /// Why: the hook read path must return the pinned slug without creating a
    /// new pin file when one already exists — same authoritative result as the
    /// writing variant but with no side-effects.
    /// What: create a project root with a pin file, call `project_slug_at_readonly`
    /// from a subdirectory, assert the pinned slug is returned and no new file
    /// is written.
    /// Test: itself.
    #[test]
    fn project_slug_at_readonly_reads_existing_pin() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("some-dir");
        fs::create_dir_all(root.join(".git")).unwrap();
        let pin = ProjectPin {
            schema_version: PIN_SCHEMA_VERSION,
            palace: "canonical-slug".to_string(),
            note: None,
        };
        write_project_pin(&root, &pin).expect("write pin");

        let sub = root.join("nested");
        fs::create_dir_all(&sub).unwrap();
        let slug = project_slug_at_readonly(&sub).expect("slug");
        assert_eq!(
            slug, "canonical-slug",
            "readonly path must return the pinned slug"
        );
    }

    /// Why: the hook read path must NOT create a pin file when none exists — the
    /// lazy-write side-effect is only appropriate for interactive commands.
    /// What: create a project root with no pin file, call `project_slug_at_readonly`,
    /// assert the basename slug is returned but the pin file is NOT created.
    /// Test: itself.
    #[test]
    fn project_slug_at_readonly_no_write_when_absent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("my-repo");
        fs::create_dir_all(root.join(".git")).unwrap();

        // No pin file before the call.
        assert!(
            read_project_pin(&root).expect("no err").is_none(),
            "no pin before call"
        );

        let slug = project_slug_at_readonly(&root).expect("slug");
        assert_eq!(slug, "my-repo", "should derive from basename");

        // Pin file must NOT have been created.
        assert!(
            read_project_pin(&root).expect("no err").is_none(),
            "pin file must NOT be written by the readonly variant"
        );
    }

    /// Why: `project_slug_at_readonly` must walk upward just like the writing
    /// variant so it works from any subdirectory, not just the project root.
    /// What: create a project root with a pin, start from a deep subdirectory,
    /// assert the pinned slug is returned.
    /// Test: itself.
    #[test]
    fn project_slug_at_readonly_falls_back_to_basename() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("basename-project");
        fs::create_dir_all(root.join(".git")).unwrap();
        // No pin file — readonly path must fall back to basename.
        let slug = project_slug_at_readonly(&root).expect("slug");
        assert_eq!(slug, "basename-project");
        // Still no pin file.
        assert!(read_project_pin(&root).unwrap().is_none());
    }

    // -----------------------------------------------------------------------
    // Change 2: validate_palace_name with pin-file cwd
    // -----------------------------------------------------------------------

    /// Why: Change 2 — when the caller passes a `cwd` path that contains
    /// (or is above) a `.trusty-tools/trusty-memory.yaml` pin file,
    /// `validate_palace_name` must accept the pinned slug rather than the
    /// basename of the CWD directory. This is the core correctness guarantee
    /// for multi-checkout and drive-reorg scenarios.
    /// What: create a project root named `new-name` with a `.git` marker and
    /// a pin file for `original-slug`; assert `validate_palace_name(
    /// "original-slug", new-name/src)` returns `Ok(())`.
    /// Test: itself.
    #[test]
    fn validate_palace_name_accepts_pinned_slug_via_cwd() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("new-name");
        fs::create_dir_all(root.join(".git")).unwrap();
        let pin = ProjectPin {
            schema_version: PIN_SCHEMA_VERSION,
            palace: "original-slug".to_string(),
            note: None,
        };
        write_project_pin(&root, &pin).expect("write pin");

        let sub = root.join("src");
        fs::create_dir_all(&sub).unwrap();

        // The pinned slug must be accepted even though the dir is "new-name".
        let result = validate_palace_name("original-slug", &sub);
        assert!(
            result.is_ok(),
            "pinned slug must be accepted when cwd resolves to pin: {result:?}"
        );

        // The basename slug must be rejected (it is not in the pin file).
        let mismatch = validate_palace_name("new-name", &sub);
        assert!(
            mismatch.is_err(),
            "non-pinned name must be rejected when pin file exists"
        );
    }

    // Note: the bypass-env contract (TRUSTY_SKIP_PALACE_ENFORCEMENT=1 allows any
    // name) is covered by `dispatch_palace_create_persists` in tools.rs, which
    // sets the env var in the test harness. No unit test here — the env-var
    // bypass is a test-only escape hatch and not part of the public API contract.

    #[cfg(unix)]
    #[test]
    fn lazy_write_non_fatal_on_readonly_dir() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("ro-project");
        fs::create_dir_all(root.join(".git")).unwrap();

        // Make the root read-only so the lazy write cannot create `.trusty-tools/`.
        let mut perms = fs::metadata(&root).unwrap().permissions();
        perms.set_mode(0o555);
        fs::set_permissions(&root, perms).unwrap();

        let slug = project_slug_at(&root);
        // Restore permissions before the tempdir drops (so cleanup works).
        let mut restore = fs::metadata(&root).unwrap().permissions();
        restore.set_mode(0o755);
        fs::set_permissions(&root, restore).unwrap();

        assert!(
            slug.is_some(),
            "slug must be returned even when the pin write fails"
        );
        assert_eq!(slug.unwrap(), "ro-project");
    }
}
