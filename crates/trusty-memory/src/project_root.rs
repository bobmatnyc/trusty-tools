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
//! Phase 1 adds a pin-file convention: a project may commit
//! `.trusty-tools/trusty-memory.yaml` at its root to pin the palace slug.
//! This survives directory renames and drive reorganisations because the slug
//! no longer depends solely on the directory basename.
//!
//! Resolution order for `project_slug_at`:
//!   a. Walk up to the project root. If `.trusty-tools/trusty-memory.yaml`
//!      exists, read `palace` from it (authoritative — survives renames).
//!   b. If absent, compute the slug from the directory basename (existing
//!      logic), then lazily write `.trusty-tools/trusty-memory.yaml` so all
//!      future resolutions are stable. The lazy write is best-effort and
//!      non-fatal (read-only trees are tolerated; failures are logged to
//!      stderr).
//!
//! What: `project_slug_at` implements the resolution order above. Helpers
//! `read_project_pin`, `write_project_pin`, and `project_slug_from_basename`
//! are split out so each can be tested independently and called by the
//! `trusty-memory link` backfill command.
//! Test: `project_slug_finds_git_root`, `project_slug_returns_none_without_markers`,
//! `project_slug_uses_first_ancestor_marker`,
//! `project_slug_personal_always_allowed`,
//! `pin_file_read_when_present`, `absent_pin_writes_computed_slug`,
//! `renamed_dir_with_pin_resolves_to_original_slug`,
//! `trusty_tools_dir_is_project_marker`,
//! `lazy_write_non_fatal_on_readonly_dir`.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::messaging::slugify_string;

/// Schema version for `.trusty-tools/trusty-memory.yaml`.
///
/// Why: forward-proofing — a future phase may need to distinguish older pin
/// files that lack new fields. Hard-coding `1` now makes that migration
/// straightforward: read `schema_version`, branch on the value.
/// What: the `u32` constant `1`.
/// Test: `write_project_pin` embeds this value; `read_project_pin` accepts it.
pub const PIN_SCHEMA_VERSION: u32 = 1;

/// Relative path of the pin file within a project root.
///
/// Why: defined as a constant so every call site (`read_project_pin`,
/// `write_project_pin`, `find_project_root`) agrees on the same path and
/// tests can compare against this value instead of a bare string literal.
/// What: `".trusty-tools/trusty-memory.yaml"`.
/// Test: used in every pin-file test in this module.
pub const PIN_FILE_REL: &str = ".trusty-tools/trusty-memory.yaml";

/// The `.trusty-tools/` directory name (used as a project marker).
///
/// Why: a project that already contains `.trusty-tools/trusty-memory.yaml`
/// should be recognised as a project root even if it has no `.git` or
/// `Cargo.toml`. Adding the directory itself to `PROJECT_MARKERS` (decision
/// D5) lets `find_project_root` detect this case without special-casing.
/// What: `".trusty-tools"`.
/// Test: `trusty_tools_dir_is_project_marker`.
pub const TRUSTY_TOOLS_DIR: &str = ".trusty-tools";

/// Serialisable schema for `.trusty-tools/trusty-memory.yaml`.
///
/// Why: a typed struct with `serde` makes the YAML schema self-documenting
/// and prevents future fields from silently deserialising to wrong types.
/// What: holds `schema_version` (always 1 for Phase 1) and `palace` (the
/// pinned slug string). An optional `note` field is supported for humans who
/// want to document why the slug was pinned.
/// Test: `write_project_pin` round-trips through `read_project_pin`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectPin {
    /// Pin-file format version. Always `1` in Phase 1.
    pub schema_version: u32,
    /// The pinned palace slug — stored verbatim, no re-slugification.
    pub palace: String,
    /// Optional human note (e.g. "pinned before drive reorg 2026-06").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Sentinel palace name that is always valid regardless of project context.
///
/// Why: users operating outside any project root (global notes, exploratory
/// sessions, personal task lists) need a stable palace that can receive
/// memories without failing the project-enforcement gate. The name `personal`
/// is the single reserved identifier for this purpose.
/// What: a `&str` constant that the enforcement logic tests against before
/// applying project-slug validation.
/// Test: `project_slug_personal_always_allowed`.
pub const PERSONAL_PALACE: &str = "personal";

/// File names that mark a directory as a project root.
///
/// Why: different ecosystems use different conventions for the project root;
/// we want a single, ordered list that every part of the codebase agrees on
/// so project detection is consistent whether invoked from CLI, MCP, or
/// tests. `.git` comes first because it is the most universal signal.
/// `.trusty-tools` is included (decision D5) so a directory that already
/// carries a pin file is recognised even without a `.git` or build manifest.
/// What: an ordered slice of filenames checked by `find_project_root`. A
/// directory is considered a project root when it contains *any* of these.
/// Test: `project_slug_uses_first_ancestor_marker`,
///       `trusty_tools_dir_is_project_marker`.
pub const PROJECT_MARKERS: &[&str] = &[
    ".git",
    "Cargo.toml",
    "pyproject.toml",
    "package.json",
    "go.mod",
    ".project-root",
    TRUSTY_TOOLS_DIR,
];

/// Walk upward from `start` and return the first ancestor directory (inclusive)
/// that contains at least one project marker.
///
/// Why: keeping the filesystem walk in a dedicated helper makes both the slug
/// derivation function and the tests easier to reason about — callers get the
/// root path, not just the slug.
/// What: starts at `start`, checks for every [`PROJECT_MARKERS`] file/dir,
/// and ascends to `parent()` until a root is found or the filesystem root is
/// reached. Returns `None` when no project root is found.
/// Test: `project_slug_finds_git_root`, `project_slug_uses_first_ancestor_marker`.
pub fn find_project_root(start: &Path) -> Option<PathBuf> {
    let mut current = start.to_path_buf();
    // Canonicalize to resolve symlinks before walking (best-effort; fall back
    // to the original path if canonicalization fails, e.g. path does not exist
    // yet).
    if let Ok(canonical) = std::fs::canonicalize(&current) {
        current = canonical;
    }
    loop {
        for marker in PROJECT_MARKERS {
            if current.join(marker).exists() {
                return Some(current);
            }
        }
        // Ascend one level; stop at the filesystem root.
        match current.parent() {
            Some(parent) if parent != current => current = parent.to_path_buf(),
            _ => return None,
        }
    }
}

/// Read the palace pin from `.trusty-tools/trusty-memory.yaml` at `root`.
///
/// Why: the pin file is the authoritative source for a project's palace slug
/// when present. Reading it in a dedicated helper keeps the I/O concern
/// separate from the slug-derivation logic and makes it easy to test the
/// round-trip in isolation.
/// What: constructs the path `root/.trusty-tools/trusty-memory.yaml`, reads
/// it, and deserialises with `serde_yaml`. Returns `None` when the file does
/// not exist. Returns `Err` only on I/O or parse failures.
/// Test: `pin_file_read_when_present`, `read_project_pin_returns_none_when_absent`.
pub fn read_project_pin(root: &Path) -> Result<Option<ProjectPin>> {
    let pin_path = root.join(PIN_FILE_REL);
    match std::fs::read_to_string(&pin_path) {
        Ok(s) => {
            let pin: ProjectPin = serde_yaml::from_str(&s)
                .map_err(|e| anyhow::anyhow!("parse {}: {e}", pin_path.display()))?;
            Ok(Some(pin))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(anyhow::anyhow!("read {}: {e}", pin_path.display())),
    }
}

/// Write a palace pin to `.trusty-tools/trusty-memory.yaml` at `root`.
///
/// Why: the lazy-write path in `project_slug_at` and the explicit
/// `trusty-memory link` backfill command both need to emit the same YAML
/// schema. A single writer keeps the format consistent and avoids duplicated
/// YAML-construction logic.
/// What: creates `.trusty-tools/` if missing, serialises `pin` with
/// `serde_yaml`, and writes it atomically (write to `<file>.tmp`, then
/// rename). Returns the path that was written.
/// Test: `write_project_pin_creates_expected_yaml`,
///       `write_project_pin_round_trips_through_read`.
pub fn write_project_pin(root: &Path, pin: &ProjectPin) -> Result<PathBuf> {
    let dir = root.join(TRUSTY_TOOLS_DIR);
    std::fs::create_dir_all(&dir).map_err(|e| anyhow::anyhow!("create {}: {e}", dir.display()))?;
    let pin_path = root.join(PIN_FILE_REL);
    let tmp_path = pin_path.with_extension("yaml.tmp");
    let yaml = serde_yaml::to_string(pin).map_err(|e| anyhow::anyhow!("serialise pin: {e}"))?;
    let header = "# .trusty-tools/trusty-memory.yaml\n\
                  # This file pins the trusty-memory palace slug for this project.\n\
                  # Commit it so the linkage survives directory renames and drive reorgs.\n\
                  # Schema: https://github.com/bobmatnyc/trusty-tools (trusty-tools convention)\n\n";
    let content = format!("{header}{yaml}");
    std::fs::write(&tmp_path, &content)
        .map_err(|e| anyhow::anyhow!("write {}: {e}", tmp_path.display()))?;
    std::fs::rename(&tmp_path, &pin_path).map_err(|e| {
        anyhow::anyhow!(
            "rename {} → {}: {e}",
            tmp_path.display(),
            pin_path.display()
        )
    })?;
    Ok(pin_path)
}

/// Compute the palace slug purely from the directory basename (the pre-Phase-1
/// logic, now extracted for composability).
///
/// Why: the resolution order in `project_slug_at` needs to call the basename
/// derivation without triggering the pin-file read/write side effects. Exposing
/// this as a separate function makes both paths testable in isolation.
/// What: calls `slugify_string` on the last path component of `root`. Returns
/// `None` when the basename is empty or slugifies to an empty string.
/// Test: `project_slug_from_basename_basic`.
pub fn project_slug_from_basename(root: &Path) -> Option<String> {
    let basename = root.file_name()?.to_str()?;
    let slug = slugify_string(basename);
    if slug.is_empty() {
        None
    } else {
        Some(slug)
    }
}

/// Derive a palace slug from the project root found at or above `start`.
///
/// Why: the core of issue #88 with Phase-1 pin-file support. Palace names
/// must match the canonical slug of the project they belong to, and that slug
/// must survive directory renames. The pin file provides the stable anchor.
/// What: implements the two-step resolution order:
///   a. Walk up to the project root. If `.trusty-tools/trusty-memory.yaml`
///      exists, return `pin.palace` (authoritative — survives renames).
///   b. If absent, compute the slug via `project_slug_from_basename`, then
///      lazily write the pin file (best-effort, non-fatal) so future calls
///      always land on path (a).
/// Returns `None` when no project root is found.
/// Test: `pin_file_read_when_present`, `absent_pin_writes_computed_slug`,
///       `renamed_dir_with_pin_resolves_to_original_slug`.
pub fn project_slug_at(start: &Path) -> Option<String> {
    let root = find_project_root(start)?;

    // Step (a): check for a committed pin file.
    match read_project_pin(&root) {
        Ok(Some(pin)) => return Some(pin.palace),
        Ok(None) => {} // absent — fall through to step (b)
        Err(e) => {
            // Corrupt or unreadable pin file: log to stderr and fall through
            // to the basename derivation so memory operations are not blocked.
            tracing::warn!(
                path = %root.join(PIN_FILE_REL).display(),
                "could not read palace pin file ({e:#}); falling back to basename slug"
            );
        }
    }

    // Step (b): compute from basename and lazily write the pin file.
    let slug = project_slug_from_basename(&root)?;
    let pin = ProjectPin {
        schema_version: PIN_SCHEMA_VERSION,
        palace: slug.clone(),
        note: None,
    };
    match write_project_pin(&root, &pin) {
        Ok(path) => {
            tracing::debug!(
                slug = %slug,
                path = %path.display(),
                "wrote palace pin file (lazy init)"
            );
        }
        Err(e) => {
            // Read-only tree, insufficient permissions, etc. — non-fatal.
            tracing::warn!(
                slug = %slug,
                root = %root.display(),
                "could not write palace pin file ({e:#}); slug will remain basename-derived"
            );
        }
    }
    Some(slug)
}

/// Derive a palace slug from the project root found at or above `start`,
/// WITHOUT the lazy-write side-effect.
///
/// Why: the `prompt-context` hook runs in read-only or short-lived contexts
/// where creating `.trusty-tools/trusty-memory.yaml` would be surprising and
/// potentially disruptive. The slug is still resolved via the pin-file when
/// one already exists (step a), and falls back to the basename slug (step b)
/// without ever writing a new file. This makes `cwd_palace_slug_at` safe to
/// call unconditionally from hooks. The writing variant (`project_slug_at`)
/// remains the right choice for interactive commands (`trusty-memory link`,
/// `trusty-memory remember`) that want to stabilise the slug.
/// What: same two-step resolution as `project_slug_at` but step (b) only
/// computes and returns the basename slug — it does NOT write the pin file.
/// Returns `None` when no project root is found.
/// Test: `project_slug_at_readonly_no_write_when_absent`,
///       `project_slug_at_readonly_reads_existing_pin`,
///       `project_slug_at_readonly_falls_back_to_basename`.
pub fn project_slug_at_readonly(start: &Path) -> Option<String> {
    let root = find_project_root(start)?;

    // Step (a): if a pin file exists, use it authoritatively.
    match read_project_pin(&root) {
        Ok(Some(pin)) => return Some(pin.palace),
        Ok(None) => {} // absent — fall through to step (b)
        Err(e) => {
            // Corrupt or unreadable pin file: log to stderr and fall through
            // so the hook is not blocked.
            tracing::warn!(
                path = %root.join(PIN_FILE_REL).display(),
                "could not read palace pin file ({e:#}); falling back to basename slug (read-only)"
            );
        }
    }

    // Step (b): compute from basename — but do NOT write a pin file.
    project_slug_from_basename(&root)
}

/// Derive a palace slug for the current working directory.
///
/// Why: convenience wrapper over `project_slug_at` for callers that want
/// the "natural" project slug (CLI commands, MCP handlers, tests running
/// inside a repo).
/// What: calls `std::env::current_dir()`, propagates the error if the syscall
/// fails, then delegates to [`project_slug_at`].
/// Test: `project_slug_finds_git_root` (run from inside the trusty-tools repo
/// which is a git checkout).
pub fn project_slug() -> Result<Option<String>> {
    let cwd = std::env::current_dir().map_err(|e| anyhow::anyhow!("read cwd: {e}"))?;
    Ok(project_slug_at(&cwd))
}

/// Validate a proposed palace name against project-slug enforcement rules.
///
/// Why: palace creation in MCP tool calls and HTTP handlers must apply the
/// same enforcement logic. Centralising the check here keeps the rule in one
/// place and makes it easy to write exhaustive unit tests.
/// What: returns `Ok(())` when the name is valid; returns `Err` with a
/// human-readable message when it is not. The rules are:
///   1. `personal` is always valid (the escape hatch for non-project
///      contexts).
///   2. When a project root is detectable from `cwd`, the name must equal
///      the derived slug.
///   3. When no project root is detectable, only `personal` is allowed.
///
/// Existing palaces are **not** affected by this check; it applies only to
/// *new* palace creation requests.
/// Test: `validate_palace_name_accepts_personal`,
/// `validate_palace_name_accepts_matching_slug`,
/// `validate_palace_name_rejects_mismatch`,
/// `validate_palace_name_rejects_non_personal_without_project`.
pub fn validate_palace_name(name: &str, cwd: &Path) -> Result<()> {
    // The `personal` palace is always a valid creation target.
    if name == PERSONAL_PALACE {
        return Ok(());
    }

    match project_slug_at(cwd) {
        Some(expected) => {
            if name == expected {
                Ok(())
            } else {
                Err(anyhow::anyhow!(
                    "palace name '{name}' does not match the project slug '{expected}' \
                     (derived from {cwd}). \
                     Either use '{expected}' or use 'personal' for non-project memories.",
                    cwd = cwd.display(),
                ))
            }
        }
        None => Err(anyhow::anyhow!(
            "no project root found at or above '{cwd}'. \
             Use 'personal' for memories not tied to a project, \
             or run from inside a project directory that contains \
             a .git file, Cargo.toml, pyproject.toml, or package.json.",
            cwd = cwd.display(),
        )),
    }
}

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
