//! Which owner a session's cwd belongs to (#8988 PR 1a).
//!
//! Why: the owner file lives at `<repos_root>/<owner>/`, so a session must map
//! its cwd — a main checkout, a linked worktree, a managed `.worktrees/<id>`,
//! or a checkout outside the managed tree — to one owner.
//! What: [`owner_for`] resolves the checkout root with
//! [`crate::core::harness_root::harness_root_for`] (the cwd itself when it is
//! not in a git repo). Under `repos_root` at depth ≥ 2 the first segment is the
//! owner; at depth 1 there is none. Outside `repos_root` the owner is the one
//! the `origin` remote names, matched case-insensitively against the owner
//! directories present.
//! Test: `owner_tests.rs`.

use std::path::{Component, Path, PathBuf};

use super::schema::is_owner_name;

/// The owner `cwd` belongs to, or `None`.
///
/// Why: see the module doc.
/// What: see the module doc. Any failed probe (no origin, an unparsable URL, a
/// git error) is `None`, never a guess: no owner means no owner layer.
/// Test: `a_checkout_under_the_repos_root_belongs_to_its_first_segment`,
/// `a_linked_worktree_belongs_to_its_main_checkouts_owner`,
/// `a_depth_one_directory_has_no_owner`,
/// `a_checkout_outside_the_tree_belongs_to_its_origin_owner`,
/// `a_directory_outside_the_tree_with_no_origin_has_no_owner`.
pub fn owner_for(cwd: &Path, repos_root: &Path) -> Option<String> {
    let root = crate::core::harness_root::harness_root_for(cwd).unwrap_or_else(|| cwd.into());
    let root = canonical(&root);
    let repos_root = canonical(repos_root);

    if let Some(rest) = strip_prefix_ignore_case(&root, &repos_root) {
        let segments: Vec<_> = rest
            .components()
            .filter_map(|c| match c {
                Component::Normal(s) => s.to_str(),
                _ => None,
            })
            .collect();
        return match segments.as_slice() {
            [owner, _, ..] if is_owner_name(owner) => Some((*owner).to_string()),
            _ => None,
        };
    }

    let url = crate::core::git_origin::get_origin_url(&root).ok()??;
    let gh = trusty_common::github_path::parse_github_path(&url)?;
    if !is_owner_name(&gh.owner) {
        return None;
    }
    // A local owner directory spelled in another case is the same owner.
    let on_disk = std::fs::read_dir(&repos_root).ok().and_then(|entries| {
        entries
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .find(|name| name.eq_ignore_ascii_case(&gh.owner))
    });
    Some(on_disk.unwrap_or(gh.owner))
}

/// `path` canonicalized, or as given when it cannot be.
fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// `path` with `prefix` stripped, comparing components case-insensitively.
fn strip_prefix_ignore_case(path: &Path, prefix: &Path) -> Option<PathBuf> {
    let mut rest = path.components();
    for want in prefix.components() {
        let got = rest.next()?;
        if !got
            .as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(&want.as_os_str().to_string_lossy())
        {
            return None;
        }
    }
    Some(rest.as_path().to_path_buf())
}

#[cfg(test)]
#[path = "owner_tests.rs"]
mod tests;
