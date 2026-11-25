//! Handler for `trusty-memory link` — explicit pre-populate of the
//! `.trusty-tools/trusty-memory.yaml` pin file.
//!
//! Why: users who want to lock in the palace name *before* a drive reorg
//! (when the directory still has its original name) need a way to do so
//! explicitly. `trusty-memory link` is the only pin writer (#11823 removed the
//! lazy write) and prints what it did.
//!
//! What: resolves the project root for the given path (default: CWD), takes
//! the palace name from `--slug`, else the shared resolver's project name
//! (git `owner/repo`, else `parent/dir`; #11823), and calls
//! `write_project_pin`. The existing `palace` value is preserved when
//! `--force` is not passed and the file already exists, so a re-run without
//! `--force` is a no-op for the slug field.
//!
//! Test: `link_creates_pin_file`, `link_is_idempotent`,
//!       `link_updates_slug_with_force`, `link_pins_the_git_owner_repo_name`.

use anyhow::{Context, Result};
use colored::Colorize;
use std::path::{Path, PathBuf};

use crate::project_root::{
    PIN_FILE_REL, ProjectPin, find_project_root, read_project_pin, resolve_palace_for_project,
    write_project_pin,
};

/// Entry point for `trusty-memory link [--path <dir>] [--slug <slug>]
/// [--note <text>] [--force]`.
///
/// Why: allows users to pin the palace slug before renaming their project
/// directory, making the linkage explicit and committed to version control.
/// What: resolves the project root (walking up from `path`), takes the name
/// from `--slug` or else the shared resolver (#11823; an untrustworthy pin is
/// an error), reads any existing pin, decides whether to write a new one
/// (first time, or when `force` is true, or when `slug` differs from the
/// existing one), and prints a summary of what happened.
/// Test: `link_creates_pin_file`, `link_is_idempotent`,
///       `link_updates_slug_with_force`, `link_pins_the_git_owner_repo_name`.
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

    // Determine the slug to write. #11823: an explicit `--slug` wins (Q3);
    // otherwise the shared resolver's project name, never the folder name.
    let slug = match slug_override {
        Some(ref s) => s.clone(),
        None => resolve_palace_for_project(&start)
            .map(|project| project.id)
            .map_err(|e| {
                anyhow::anyhow!(
                    "could not resolve a palace name from '{}': {e}. \
                     Pass --slug <slug> to set one explicitly.",
                    start.display()
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

    let pin = ProjectPin::new(slug.clone()).with_note(note);

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
    use crate::project_root::PIN_SCHEMA_VERSION;
    use std::fs;

    /// Why: the primary use-case — a fresh project directory has no pin file;
    /// `handle_link` must create one with the resolver's name (#11823: the
    /// `parent/dir` slug for a repo with no remote, not the folder name).
    /// What: create a project root `work/my-new-project` with `.git`, call
    /// `handle_link` from it, read back the pin file, and assert the slug.
    /// Test: itself.
    #[test]
    fn link_creates_pin_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("work").join("my-new-project");
        fs::create_dir_all(root.join(".git")).unwrap();

        handle_link(Some(root.clone()), None, None, false).expect("handle_link ok");

        let pin = read_project_pin(&root)
            .expect("read ok")
            .expect("Some(pin)");
        assert_eq!(pin.palace, "work-my-new-project");
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
        let root = tmp.path().join("work").join("stable-project");
        fs::create_dir_all(root.join(".git")).unwrap();

        handle_link(Some(root.clone()), None, None, false).expect("first call ok");
        handle_link(Some(root.clone()), None, None, false).expect("second call ok");

        let pin = read_project_pin(&root)
            .expect("read ok")
            .expect("Some(pin)");
        assert_eq!(pin.palace, "work-stable-project");
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
        let initial = ProjectPin::new("old-slug".to_string());
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

        let initial = ProjectPin::new("guarded-slug".to_string());
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

    /// Why (#11823): `link` wrote the folder name while readers resolve git
    /// `owner/repo`, so a linked project pinned a palace no reader used.
    /// What: a git repo `My_Repo` with origin `Acme/My_Repo`; `handle_link`
    /// with no `--slug` pins `acme-my-repo`, not `my-repo`.
    /// Test: itself.
    #[test]
    fn link_pins_the_git_owner_repo_name() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("My_Repo");
        fs::create_dir_all(&root).unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["remote", "add", "origin", "git@github.com:Acme/My_Repo.git"],
        ] {
            let out = std::process::Command::new("git")
                .args(&args)
                .current_dir(&root)
                .env_remove("GIT_DIR")
                .env_remove("GIT_WORK_TREE")
                .output()
                .expect("run git");
            assert!(out.status.success(), "git {args:?}: {out:?}");
        }

        handle_link(Some(root.clone()), None, None, false).expect("handle_link ok");

        let pin = read_project_pin(&root)
            .expect("read ok")
            .expect("Some(pin)");
        assert_eq!(pin.palace, "acme-my-repo");
    }
}
