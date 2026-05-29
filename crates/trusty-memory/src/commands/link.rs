//! Handler for `trusty-memory link` — explicit pre-populate of the
//! `.trusty-tools/trusty-memory.yaml` pin file.
//!
//! Why: the lazy write in `project_slug_at` happens automatically on the
//! first memory operation inside a project, but users who want to lock in
//! the slug *before* a drive reorg (when the directory still has its
//! original name) need a way to do so explicitly. `trusty-memory link`
//! writes (or refreshes) the pin file immediately and prints what it did.
//!
//! What: resolves the project root for the given path (default: CWD),
//! computes the slug from the current directory basename, and calls
//! `write_project_pin` — even if a pin file already exists (refresh
//! semantics). The existing `palace` value is preserved when `--force` is
//! not passed and the file already exists, so a re-run without `--force`
//! is a no-op for the slug field.
//!
//! Test: `link_creates_pin_file`, `link_is_idempotent`,
//!       `link_updates_slug_with_force`.

use anyhow::{Context, Result};
use colored::Colorize;
use std::path::{Path, PathBuf};

use crate::project_root::{
    find_project_root, project_slug_from_basename, read_project_pin, write_project_pin, ProjectPin,
    PIN_FILE_REL, PIN_SCHEMA_VERSION,
};

/// Entry point for `trusty-memory link [--path <dir>] [--slug <slug>]
/// [--note <text>] [--force]`.
///
/// Why: allows users to pin the palace slug before renaming their project
/// directory, making the linkage explicit and committed to version control.
/// What: resolves the project root (walking up from `path`), reads any
/// existing pin, decides whether to write a new one (first time, or when
/// `force` is true, or when `slug` differs from the existing one), and
/// prints a summary of what happened.
/// Test: `link_creates_pin_file`, `link_is_idempotent`,
///       `link_updates_slug_with_force`.
pub fn handle_link(
    path: Option<PathBuf>,
    slug_override: Option<String>,
    note: Option<String>,
    force: bool,
) -> Result<()> {
    let start = match path {
        Some(p) => p,
        None => std::env::current_dir().context("could not read current directory")?,
    };

    // Resolve the project root.
    let root = find_project_root(&start).ok_or_else(|| {
        anyhow::anyhow!(
            "no project root found at or above '{}'. \
             A project root must contain one of: .git, Cargo.toml, pyproject.toml, \
             package.json, go.mod, .project-root, or .trusty-tools/.",
            start.display()
        )
    })?;

    // Determine the slug to write.
    let slug = match slug_override {
        Some(ref s) => s.clone(),
        None => project_slug_from_basename(&root).ok_or_else(|| {
            anyhow::anyhow!(
                "could not derive a palace slug from '{}'. \
                 Pass --slug <slug> to set one explicitly.",
                root.display()
            )
        })?,
    };

    // Check for an existing pin file.
    let existing = read_project_pin(&root)
        .with_context(|| format!("read existing pin at {}", root.join(PIN_FILE_REL).display()))?;

    let pin_path = root.join(PIN_FILE_REL);

    match existing {
        Some(ref existing_pin) if !force && existing_pin.palace == slug => {
            // Idempotent — same slug, no force flag.
            println!(
                "{} {} already pinned to palace '{}' (use --force to overwrite).",
                "·".dimmed(),
                pin_path.display().to_string().dimmed(),
                slug.cyan()
            );
            return Ok(());
        }
        Some(ref existing_pin) if !force => {
            // Different slug and no --force — warn and bail.
            println!(
                "{} {} already exists with palace '{}'. \
                 Pass --force to overwrite with '{}'.",
                "!".yellow(),
                pin_path.display(),
                existing_pin.palace.cyan(),
                slug.cyan()
            );
            return Ok(());
        }
        _ => {} // Absent or --force: proceed.
    }

    let pin = ProjectPin {
        schema_version: PIN_SCHEMA_VERSION,
        palace: slug.clone(),
        note,
    };

    write_project_pin(&root, &pin)
        .with_context(|| format!("write pin to {}", pin_path.display()))?;

    let action = if existing.is_some() {
        "Updated"
    } else {
        "Created"
    };
    println!(
        "{} {} {} (palace = '{}').",
        "✓".green(),
        action,
        pin_path.display(),
        slug.cyan()
    );
    println!("  Commit this file to lock the palace linkage across directory renames.");
    Ok(())
}

/// Resolve the project root for the `link` command and return it (for tests
/// and for `doctor --fix-palaces` integration).
///
/// Why: exposing the root-resolution step as a standalone helper lets the
/// doctor command reuse it without duplicating the error message.
/// What: delegates to `find_project_root`.
/// Test: covered by `link_creates_pin_file` (implicitly).
pub fn resolve_link_root(path: &Path) -> Option<PathBuf> {
    find_project_root(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Why: the primary use-case — a fresh project directory has no pin file;
    /// `handle_link` must create one with the correct slug.
    /// What: create a project root with `.git`, call `handle_link` from it,
    /// read back the pin file, and assert the slug matches.
    /// Test: itself.
    #[test]
    fn link_creates_pin_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("my-new-project");
        fs::create_dir_all(root.join(".git")).unwrap();

        handle_link(Some(root.clone()), None, None, false).expect("handle_link ok");

        let pin = read_project_pin(&root)
            .expect("read ok")
            .expect("Some(pin)");
        assert_eq!(pin.palace, "my-new-project");
        assert_eq!(pin.schema_version, PIN_SCHEMA_VERSION);
    }

    /// Why: running `link` twice against the same directory must be a no-op
    /// so it is safe to include in a setup script.
    /// What: call `handle_link` twice; assert both calls succeed and the pin
    /// file still contains the original slug.
    /// Test: itself.
    #[test]
    fn link_is_idempotent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("stable-project");
        fs::create_dir_all(root.join(".git")).unwrap();

        handle_link(Some(root.clone()), None, None, false).expect("first call ok");
        handle_link(Some(root.clone()), None, None, false).expect("second call ok");

        let pin = read_project_pin(&root)
            .expect("read ok")
            .expect("Some(pin)");
        assert_eq!(pin.palace, "stable-project");
    }

    /// Why: `--force` with an explicit slug must overwrite an existing pin.
    /// What: create a root, write an initial pin with slug `"old-slug"`, then
    /// call `handle_link` with `--slug new-slug --force` and assert the pin
    /// file was updated.
    /// Test: itself.
    #[test]
    fn link_updates_slug_with_force() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("proj");
        fs::create_dir_all(root.join(".git")).unwrap();

        // Write an initial pin.
        let initial = ProjectPin {
            schema_version: PIN_SCHEMA_VERSION,
            palace: "old-slug".to_string(),
            note: None,
        };
        write_project_pin(&root, &initial).expect("initial write ok");

        // Re-link with a different slug + --force.
        handle_link(Some(root.clone()), Some("new-slug".to_string()), None, true)
            .expect("forced update ok");

        let pin = read_project_pin(&root)
            .expect("read ok")
            .expect("Some(pin)");
        assert_eq!(pin.palace, "new-slug", "slug must be updated");
    }

    /// Why: attempting to overwrite an existing pin without `--force` must
    /// NOT update the file (guard against accidental overwrites).
    /// What: write an initial pin with `"guarded-slug"`, call `handle_link`
    /// with a different slug but no `--force`, and assert the file still
    /// contains `"guarded-slug"`.
    /// Test: itself.
    #[test]
    fn link_refuses_overwrite_without_force() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("safe-project");
        fs::create_dir_all(root.join(".git")).unwrap();

        let initial = ProjectPin {
            schema_version: PIN_SCHEMA_VERSION,
            palace: "guarded-slug".to_string(),
            note: None,
        };
        write_project_pin(&root, &initial).expect("initial write ok");

        // Attempt to overwrite without --force.
        handle_link(
            Some(root.clone()),
            Some("interloper-slug".to_string()),
            None,
            false,
        )
        .expect("handle_link returns Ok (non-fatal guard)");

        let pin = read_project_pin(&root)
            .expect("read ok")
            .expect("Some(pin)");
        assert_eq!(pin.palace, "guarded-slug", "slug must not change");
    }
}
