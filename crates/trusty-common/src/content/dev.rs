//! The dev override: read content straight from a trusty-tools checkout.

use std::path::{Path, PathBuf};

use super::ContentError;

/// Where each content class lives in a trusty-tools checkout, relative to its
/// root, in the order `scripts/package_content.sh` packages them.
///
/// Why: ADR-0064 decision 5 (iii) — run from inside the checkout, `tm` reads
/// the working tree, not the installed cache. Until PHASE_1 (#8387) moves the
/// assets to `content/<class>/`, the classes live in today's in-crate
/// directories; this table mirrors the packager's `LEGACY_SOURCES`, so a
/// bundle path (`skills/tm/SKILL.md`) names the same file in both modes.
/// PHASE_1 changes the right-hand column to `content/<class>` and nothing else.
/// Test: `dev_class_table_matches_the_packager` pins it to the packager.
pub const DEV_CLASS_SOURCES: &[(&str, &str)] = &[
    ("agents", "crates/trusty-agents-common/src/assets/agents"),
    ("skills", "crates/trusty-mpm/src/assets/skills"),
    ("instructions", "crates/trusty-mpm/src/assets/instructions"),
    (
        "output-styles",
        "crates/trusty-mpm/src/assets/output-styles",
    ),
    (
        "sm_instructions",
        "crates/trusty-mpm/src/assets/sm_instructions",
    ),
    (
        "harness_understanding",
        "crates/trusty-agents-common/src/assets/harness_understanding",
    ),
];

/// Returns the nearest ancestor of `start` (itself included) that holds every
/// directory in [`DEV_CLASS_SOURCES`], or `None` outside any checkout.
///
/// Why: "inside the trusty-tools checkout" has to be decided from the tree
/// itself; requiring every class directory means a partial tree, or an
/// unrelated repository, is never mistaken for one.
/// Test: `dev_checkout_is_found_from_a_nested_directory`,
/// `dev_checkout_is_not_found_outside_a_checkout`.
pub fn find_dev_checkout(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|dir| first_missing_class(dir).is_none())
        .map(Path::to_path_buf)
}

/// Checks that `root` holds every class directory, naming the first missing one.
pub(super) fn require_checkout(root: &Path) -> Result<(), ContentError> {
    match first_missing_class(root) {
        None => Ok(()),
        Some(missing) => Err(ContentError::NotACheckout {
            root: root.to_path_buf(),
            missing,
        }),
    }
}

fn first_missing_class(root: &Path) -> Option<PathBuf> {
    DEV_CLASS_SOURCES
        .iter()
        .map(|(_, rel)| root.join(rel))
        .find(|dir| !dir.is_dir())
}

/// Maps a validated bundle path (`<class>/<rest>`) to its checkout file.
pub(super) fn file_for(root: &Path, key: &str) -> Option<PathBuf> {
    let (class, rest) = key.split_once('/')?;
    class_dir(root, class).map(|dir| dir.join(rest))
}

/// The checkout directory holding `class`, if the class is known.
pub(super) fn class_dir(root: &Path, class: &str) -> Option<PathBuf> {
    DEV_CLASS_SOURCES
        .iter()
        .find(|(name, _)| *name == class)
        .map(|(_, rel)| root.join(rel))
}

/// Lists every regular file under `dir` as `<class>/<relative path>`, skipping
/// dot-files and symlinks exactly as the packager does.
pub(super) fn list_class(dir: &Path, class: &str) -> Result<Vec<String>, ContentError> {
    let mut out = Vec::new();
    walk(dir, class, &mut out)?;
    out.sort();
    Ok(out)
}

fn walk(dir: &Path, prefix: &str, out: &mut Vec<String>) -> Result<(), ContentError> {
    let io_err = |source| ContentError::Io {
        path: dir.to_path_buf(),
        source,
    };
    for entry in std::fs::read_dir(dir).map_err(io_err)? {
        let entry = entry.map_err(io_err)?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.starts_with('.') {
            continue;
        }
        let kind = entry.file_type().map_err(io_err)?;
        let key = format!("{prefix}/{name}");
        if kind.is_dir() {
            walk(&entry.path(), &key, out)?;
        } else if kind.is_file() {
            out.push(key);
        }
    }
    Ok(())
}
