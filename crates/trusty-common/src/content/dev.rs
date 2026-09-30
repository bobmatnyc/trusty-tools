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

/// The repository marker; a file in a linked worktree, a directory otherwise.
const GIT_MARKER: &str = ".git";

/// The workspace manifest a checkout root carries.
const WORKSPACE_MANIFEST: &str = "Cargo.toml";

/// Returns the enclosing repository root of `start` when it is a trusted
/// trusty-tools checkout, or `None`.
///
/// Why: content read from a checkout becomes PM instructions, so a stray tree
/// must never qualify. Trusting any ancestor that merely held the class
/// directories let a world-writable ancestor such as `/tmp` plant them (the
/// git CVE-2022-24765 class; #8378 review).
/// What: climbs from `start` to the FIRST ancestor holding `.git` — the
/// repository boundary — and never past it. That one directory is the only
/// candidate, and it must pass every check [`DevOverride::At`] applies: all
/// class directories, a `Cargo.toml` with a `[workspace]` table, and (on unix)
/// a root, `.git` and `Cargo.toml` owned by the current effective uid in a root
/// no other user can write to.
/// Test: `dev_checkout_is_found_from_a_nested_directory`,
/// `dev_checkout_is_found_from_a_nested_directory_of_a_linked_worktree`,
/// `dev_checkout_is_not_found_outside_a_checkout`,
/// `dev_checkout_stops_at_the_repository_boundary`,
/// `dev_checkout_requires_a_git_marker`,
/// `dev_checkout_requires_a_workspace_manifest`,
/// `dev_checkout_refuses_a_root_owned_by_another_user`,
/// `dev_checkout_refuses_a_world_writable_root`.
///
/// [`DevOverride::At`]: super::DevOverride::At
pub fn find_dev_checkout(start: &Path) -> Option<PathBuf> {
    find_dev_checkout_as(start, current_euid())
}

/// [`find_dev_checkout`] for an explicit effective uid (`None`: no ownership
/// model, i.e. not unix).
pub(super) fn find_dev_checkout_as(start: &Path, euid: Option<u32>) -> Option<PathBuf> {
    let root = start
        .ancestors()
        .find(|dir| dir.join(GIT_MARKER).exists())?;
    check_checkout(root, euid).ok().map(|()| root.to_path_buf())
}

/// Checks that `root` is a trusted checkout, as the current user.
pub(super) fn require_checkout(root: &Path) -> Result<(), ContentError> {
    check_checkout(root, current_euid())
}

/// Reads the owning uid from a path's metadata; `None` when the platform has
/// no uid. A test substitutes one to fake a file another user owns.
pub(super) type OwnerFn = dyn Fn(&Path, &std::fs::Metadata) -> Option<u32>;

/// [`check_checkout_with`] reading real owners.
pub(super) fn check_checkout(root: &Path, euid: Option<u32>) -> Result<(), ContentError> {
    check_checkout_with(root, euid, &file_owner)
}

/// Every class directory present (else `NotACheckout`), then the `.git`
/// marker, the ownership and mode rules and the workspace manifest (else
/// `UntrustedCheckout`). Ownership is checked before `Cargo.toml` is read, so a
/// file in a foreign tree is never parsed.
///
/// Test: `dev_checkout_refuses_a_marker_owned_by_another_user`,
/// `dev_checkout_refuses_a_world_writable_root`.
pub(super) fn check_checkout_with(
    root: &Path,
    euid: Option<u32>,
    owner: &OwnerFn,
) -> Result<(), ContentError> {
    if let Some(missing) = first_missing_class(root) {
        return Err(ContentError::NotACheckout {
            root: root.to_path_buf(),
            missing,
        });
    }
    let untrusted = |reason: String| ContentError::UntrustedCheckout {
        root: root.to_path_buf(),
        reason,
    };
    if !root.join(GIT_MARKER).exists() {
        return Err(untrusted(format!("it has no {GIT_MARKER}")));
    }
    check_owner(root, euid, owner).map_err(untrusted)?;
    if !is_workspace_manifest(&root.join(WORKSPACE_MANIFEST)) {
        return Err(untrusted(format!(
            "its {WORKSPACE_MANIFEST} has no [workspace] table"
        )));
    }
    Ok(())
}

/// Whether `path` parses as TOML with a `[workspace]` table.
fn is_workspace_manifest(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| text.parse::<toml::Table>().ok())
        .is_some_and(|table| table.get("workspace").is_some_and(toml::Value::is_table))
}

/// Refuses a root that every user can write to, or whose root, `.git` or
/// `Cargo.toml` is not owned by `euid`; a no-op when `euid` is `None`.
///
/// Why: owning the root alone let another user plant `.git` and `Cargo.toml`
/// in a directory anyone can write to — `/tmp` qualifies for root (#8378
/// review). Group write stays allowed: a user-private-group umask of 002 sets
/// it on ordinary checkouts, and that group is the user's own.
/// What: the root is `stat`ed through a symlink (a checkout may be reached by
/// one) and refused when its mode has `o+w`; the root and both markers must be
/// owned by `euid`, the markers read with `lstat` so a symlink is judged by
/// who planted it.
fn check_owner(root: &Path, euid: Option<u32>, owner: &OwnerFn) -> Result<(), String> {
    let Some(euid) = euid else { return Ok(()) };
    let unreadable = |path: &Path, e: std::io::Error| {
        format!("the owner of {} cannot be read: {e}", path.display())
    };
    let root_meta = std::fs::metadata(root).map_err(|e| unreadable(root, e))?;
    if world_writable(&root_meta) {
        return Err("it is writable by every user".to_owned());
    }
    let git = root.join(GIT_MARKER);
    let manifest = root.join(WORKSPACE_MANIFEST);
    let git_meta = std::fs::symlink_metadata(&git).map_err(|e| unreadable(&git, e))?;
    let manifest_meta =
        std::fs::symlink_metadata(&manifest).map_err(|e| unreadable(&manifest, e))?;
    for (label, path, meta) in [
        ("it", root, &root_meta),
        (GIT_MARKER, git.as_path(), &git_meta),
        (WORKSPACE_MANIFEST, manifest.as_path(), &manifest_meta),
    ] {
        let uid = owner(path, meta).ok_or_else(|| format!("the owner of {label} is unknown"))?;
        if uid != euid {
            return Err(format!(
                "{label} is owned by uid {uid}, not the current user (uid {euid})"
            ));
        }
    }
    Ok(())
}

/// The owning uid in `meta`.
#[cfg(unix)]
pub(super) fn file_owner(_path: &Path, meta: &std::fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    Some(meta.uid())
}

/// No uid on this platform.
#[cfg(not(unix))]
pub(super) fn file_owner(_path: &Path, _meta: &std::fs::Metadata) -> Option<u32> {
    None
}

/// Whether every user may write to the file `meta` describes.
#[cfg(unix)]
fn world_writable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o002 != 0
}

/// No mode bits on this platform.
#[cfg(not(unix))]
fn world_writable(_meta: &std::fs::Metadata) -> bool {
    false
}

/// The process's effective uid on unix; `None` elsewhere.
#[cfg(unix)]
pub(super) fn current_euid() -> Option<u32> {
    // SAFETY: geteuid(2) takes no arguments, touches no memory and cannot fail.
    Some(unsafe { libc::geteuid() })
}

/// The process's effective uid on unix; `None` elsewhere.
#[cfg(not(unix))]
pub(super) fn current_euid() -> Option<u32> {
    None
}

fn first_missing_class(root: &Path) -> Option<PathBuf> {
    DEV_CLASS_SOURCES
        .iter()
        .map(|(_, rel)| root.join(rel))
        .find(|dir| !dir.is_dir())
}

/// Reads the checkout file behind a validated bundle path (`<class>/<rest>`).
///
/// Why: a bundle holds only regular files, so dev mode serves only what a
/// bundle could hold — a symlink could otherwise point a skill anywhere on
/// disk (#8378 review).
/// What: every component of `<rest>` is checked with `symlink_metadata`: each
/// directory must be a real directory and the last a regular file. `Ok(None)`
/// when the path is absent, is a symlink, a directory or anything else, or
/// when any component starts with `.` — the packager skips those, as
/// [`list_class`] does.
/// Test: `dev_read_serves_only_regular_files`.
pub(super) fn read_regular(root: &Path, key: &str) -> Result<Option<Vec<u8>>, ContentError> {
    let Some((class, rest)) = key.split_once('/') else {
        return Ok(None);
    };
    let Some(mut path) = class_dir(root, class) else {
        return Ok(None);
    };
    let mut parts = rest.split('/').peekable();
    while let Some(part) = parts.next() {
        // #8378 review: a bundle never holds a dot-file, so dev mode must not.
        if part.starts_with('.') {
            return Ok(None);
        }
        path.push(part);
        let kind = match std::fs::symlink_metadata(&path) {
            Ok(meta) => meta.file_type(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(ContentError::Io { path, source }),
        };
        let expected = if parts.peek().is_some() {
            kind.is_dir()
        } else {
            kind.is_file()
        };
        if !expected {
            return Ok(None);
        }
    }
    match std::fs::read(&path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(ContentError::Io { path, source }),
    }
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
