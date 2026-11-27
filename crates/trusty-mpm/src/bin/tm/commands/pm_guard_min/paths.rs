//! Path facts for the minimal pm-guard (#11431): resolution, canonical
//! form, the trust-anchor list (rule 1) and the protected-path list (rule 2).
//!
//! Why: a rule that compares spelled paths is beaten by a symlink, a `..` or
//! a `$F`; one that compares canonical paths is not.
//! What: [`resolve`] joins and normalises lexically, [`canon`] resolves
//! symlinks through the longest existing ancestor, [`is_trust_anchor`] and
//! [`hits_protected`] test a canonical path.
//! Test: `facts_tests.rs` (`anchors_resolve_through_symlinks`,
//! `protected_covers_ancestors_and_globbed_contents`).

use std::path::{Component, Path, PathBuf};

use trusty_common::install_paths::{MPM_DIR_NAME, TOOLS_DIR_NAME};

/// Join `word` to `cwd` when relative and drop `.`/`..` lexically.
pub(crate) fn resolve(cwd: &Path, word: &str) -> PathBuf {
    let joined = cwd.join(word);
    let mut out = PathBuf::new();
    for part in joined.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// The canonical path: symlinks resolved on the longest existing ancestor.
///
/// Why: a Write may create a file that does not exist yet, and
/// `std::fs::canonicalize` refuses a missing path.
/// What: canonicalises the deepest existing ancestor and re-appends the rest.
/// Test: `anchors_resolve_through_symlinks`.
pub(crate) fn canon(path: &Path) -> PathBuf {
    let mut rest = Vec::new();
    let mut cur = path.to_path_buf();
    loop {
        if let Ok(real) = std::fs::canonicalize(&cur) {
            return rest.iter().rev().fold(real, |acc, part| acc.join(part));
        }
        match (cur.file_name().map(ToOwned::to_owned), cur.parent()) {
            (Some(name), Some(parent)) => {
                rest.push(name);
                cur = parent.to_path_buf();
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// Rule 1: is the canonical `path` a trust anchor?
///
/// Why: these files decide whether the guard runs and what it runs as.
/// What: any `.claude/settings*.json`, the `tm`/`trusty-mpm` binaries in
/// `~/.cargo/bin`, `~/.trusty-mpm/config.toml`, `~/.trusty-tools/trusty-mpm/config.yaml`, any file directly under
/// `~/Library/LaunchAgents`, and any `.plist` under `~/Library` (the brief's §2
/// mitigation for #8439, widened by the #11431 must-catch corpus). `no_guard_entry` excludes a project-tier
/// settings write the caller knows touches no guard hook entry.
/// Test: `r1_flags_writes_to_each_anchor`.
pub(crate) fn is_trust_anchor(path: &Path, home: &Path, no_guard_entry: bool) -> bool {
    let home = canon(home);
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let parent = path.parent().unwrap_or(Path::new("/"));
    if parent.file_name().is_some_and(|p| p == ".claude")
        && name.starts_with("settings")
        && name.ends_with(".json")
    {
        return parent == home.join(".claude") || !no_guard_entry;
    }
    let cargo_bin = home.join(".cargo/bin");
    // #11821 P3: the configured config dir's anchors too, and install.toml,
    // which relocates that dir; the `$HOME` anchors stay for a broken file.
    let cfg = canon(&trusty_mpm::core::config_base::config_base_under(&home));
    // #11821: the dir names come from the shared install_paths constants.
    (parent == cargo_bin && matches!(name, "tm" | "trusty-mpm"))
        || path == home.join(MPM_DIR_NAME).join("config.toml")
        || path == home.join(TOOLS_DIR_NAME).join("trusty-mpm/config.yaml")
        || path == cfg.join(MPM_DIR_NAME).join("config.toml")
        || path == cfg.join(TOOLS_DIR_NAME).join("trusty-mpm/config.yaml")
        || path == trusty_common::install_paths::install_file_path(&home)
        || parent == home.join("Library/LaunchAgents")
        || (path.starts_with(home.join("Library")) && name.ends_with(".plist"))
}

/// Rule 2: would removing canonical `target` remove a protected path?
///
/// Why: deleting a parent deletes the child, so an ancestor of the home dir
/// or of a repo root is as protected as the root itself.
/// What: `true` when `target` equals or contains any of `protected`. A
/// target with a glob (`~/*`) is judged by the directory before the glob:
/// emptying a protected directory counts.
/// Test: `protected_covers_ancestors_and_globbed_contents`.
pub(crate) fn hits_protected(target: &Path, protected: &[PathBuf]) -> bool {
    let globbed = target.components().position(|c| {
        c.as_os_str()
            .to_str()
            .is_some_and(|s| s.contains(['*', '?', '[']))
    });
    let base: PathBuf = match globbed {
        Some(i) => target.components().take(i).collect(),
        None => target.to_path_buf(),
    };
    let base = canon(&base);
    protected.iter().any(|p| canon(p).starts_with(&base))
}
