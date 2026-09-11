//! Which registry rows a user surface may offer as a place to work (#7406).
//!
//! Why: the registry is an upsert store that anything can write to — a probe, a
//! test harness, an agent working in a scratch directory — so it accumulates
//! rows that name a directory which is temporary, already deleted, or was never
//! a checkout. Offering one of those as a place to start a session can only
//! fail, and the owner saw exactly that: a `mcp-probe-scratch-4181` row
//! registered at a `/private/tmp/…/scratchpad/…` path sitting in the `tm ls`
//! new-session picker.
//!
//! What: [`is_offerable_project`] is the ONE predicate every surface that
//! offers a project to an operator routes through. It judges only rows whose
//! `repo_url` is a local absolute PATH; a remote URL is always offerable,
//! because nothing on this host can contradict it.
//!
//! `tm project list` deliberately does NOT filter (#7406): it is the surface an
//! operator uses to SEE the registry and remove a bad row, so hiding rows there
//! would hide the thing being pruned. Only surfaces that offer a project as a
//! target for new work call this.
//!
//! Test: `offerable_*` in this file's `tests` module.

use std::path::{Path, PathBuf};

use trusty_mpm::project::Project;

/// Path segment that marks an agent's scratch directory.
const SCRATCHPAD_SEGMENT: &str = "scratchpad";

/// Absolute prefixes under which every registration is throwaway.
const TEMP_PREFIXES: [&str; 3] = ["/tmp", "/private/tmp", "/var/folders"];

/// True when this registry row may be offered as a place to start work.
///
/// Why: see the module doc — an unusable row in a picker is worse than a
/// missing one, because the failure only arrives after the operator commits to
/// it.
/// What: a `repo_url` that is not a local absolute path (a git URL, or empty)
/// is always offerable. A local path is offerable only when it is outside every
/// temp directory, carries no `scratchpad` segment, still exists, and sits in a
/// git checkout.
/// Test: `offerable_keeps_url_projects`, `offerable_drops_a_temp_scratchpad`,
/// `offerable_drops_a_path_that_is_gone`, `offerable_keeps_a_real_checkout`.
pub(crate) fn is_offerable_project(project: &Project) -> bool {
    match local_checkout_path(&project.repo_url) {
        Some(path) => is_offerable_checkout(&path),
        None => true,
    }
}

/// The local directory a `repo_url` names, when it names one at all.
///
/// A registry row's `repo_url` is a git URL for a cloned project and an
/// absolute checkout path for a locally-registered one; only the second kind
/// can be checked against this host's disk.
pub(crate) fn local_checkout_path(repo_url: &str) -> Option<PathBuf> {
    let trimmed = repo_url.trim();
    trimmed.starts_with('/').then(|| PathBuf::from(trimmed))
}

/// True when `path` is a durable git checkout an operator would work in.
///
/// Why: split from [`is_offerable_project`] so the four rules can be read (and
/// tested) against a bare path, without a `Project` around it.
/// What: rejects a temp-directory path, a `scratchpad` segment, a path that is
/// no longer a directory, and a directory with no `.git` at or above it.
/// Test: `offerable_drops_a_temp_scratchpad`, `offerable_drops_a_path_that_is_gone`,
/// `offerable_keeps_a_real_checkout`.
pub(crate) fn is_offerable_checkout(path: &Path) -> bool {
    !under_temp_dir(path) && !has_scratchpad_segment(path) && path.is_dir() && in_git_checkout(path)
}

/// True when `path` is inside a temp directory, `$TMPDIR` included.
///
/// `Path::starts_with` compares whole components, so `/tmpfoo` is not `/tmp`.
fn under_temp_dir(path: &Path) -> bool {
    let tmpdir = std::env::var_os("TMPDIR").map(PathBuf::from);
    TEMP_PREFIXES
        .iter()
        .map(Path::new)
        .any(|prefix| path.starts_with(prefix))
        || tmpdir.is_some_and(|dir| {
            let dir = dir.to_string_lossy().trim_end_matches('/').to_string();
            !dir.is_empty() && path.starts_with(&dir)
        })
}

/// True when any component of `path` is a `scratchpad` directory.
fn has_scratchpad_segment(path: &Path) -> bool {
    path.components().any(|c| {
        c.as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(SCRATCHPAD_SEGMENT)
    })
}

/// True when `path` or one of its ancestors holds a `.git` entry.
///
/// A git worktree's `.git` is a FILE rather than a directory, and a registered
/// path can be a subdirectory of the checkout, so both shapes count.
fn in_git_checkout(path: &Path) -> bool {
    path.ancestors().any(|dir| dir.join(".git").exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A registry row, built the way the daemon's own listing yields one.
    fn project(name: &str, repo_url: &str) -> Project {
        serde_json::from_value(serde_json::json!({
            "name": name,
            "repo_url": repo_url,
            "default_branch": "main",
        }))
        .expect("project fixture")
    }

    /// The acceptance fixture (#7406): a temp scratchpad row, a row whose path
    /// is gone, and two rows that must survive.
    fn fixture() -> Vec<Project> {
        vec![
            project(
                "mcp-probe-scratch-4181",
                "/private/tmp/claude-502/abc/scratchpad/mcp-probe-scratch-4181",
            ),
            project("gone", "/nonexistent/7406/no-such-checkout"),
            project("trusty-tools", "https://github.com/bobmatnyc/trusty-tools"),
            project("apex", "git@github.com:duetto/apex.git"),
        ]
    }

    #[test]
    fn offerable_drops_the_throwaway_rows_and_keeps_the_rest() {
        let rows = fixture();
        let kept: Vec<&str> = rows
            .iter()
            .filter(|p| is_offerable_project(p))
            .map(|p| p.name.as_str())
            .collect();
        assert_eq!(kept, vec!["trusty-tools", "apex"]);
    }

    #[test]
    fn offerable_keeps_url_projects() {
        assert!(is_offerable_project(&project(
            "t",
            "https://github.com/bobmatnyc/trusty-tools"
        )));
        // An empty `repo_url` names no directory, so there is nothing to judge.
        assert!(is_offerable_project(&project("t", "")));
    }

    #[test]
    fn offerable_drops_a_temp_scratchpad() {
        assert!(!is_offerable_checkout(Path::new("/private/tmp/x/y")));
        assert!(!is_offerable_checkout(Path::new("/tmp/x")));
        assert!(!is_offerable_checkout(Path::new("/var/folders/ab/cd/T/x")));
        // A `scratchpad` segment is throwaway wherever it sits.
        assert!(!is_offerable_checkout(Path::new(
            "/Users/me/work/scratchpad/probe"
        )));
        // The prefix match is by component, not by string.
        assert!(!under_temp_dir(Path::new("/tmpfoo/bar")));
    }

    #[test]
    fn offerable_drops_a_path_that_is_gone() {
        assert!(!is_offerable_checkout(Path::new(
            "/nonexistent/7406/no-such-checkout"
        )));
    }

    #[test]
    fn offerable_keeps_a_real_checkout() {
        // This crate's own source directory is a subdirectory of a checkout
        // whose `.git` may be a file (a worktree) or a directory.
        let here = Path::new(env!("CARGO_MANIFEST_DIR"));
        assert!(in_git_checkout(here), "{here:?} is not in a checkout");
        assert!(is_offerable_checkout(here), "{here:?} was rejected");
    }
}
