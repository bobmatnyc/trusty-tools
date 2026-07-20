//! Known-project roster discovery for the GUI's workstream-first
//! project-picker modal (issue #3365).
//!
//! Why: DOC-48 §8/DOC-39 §4.2 shift the GUI's entry concept from "new
//! session" to "new workstream" — creating a workstream now binds it to a
//! project through a MODAL, not [`super::list_dir`]'s arbitrary-directory
//! browse-and-descend picker (7a). A modal wants a short, flat, best-guess
//! candidate LIST to click through, not a filesystem tree to navigate. The
//! prior GUI slice's own module doc recorded this exact gap: "no pre-session
//! roster endpoint exists yet" — this module is that endpoint's daemon-side
//! data source. There is no existing project/config registry in this crate
//! to reuse (`crate::binding::ProjectBinding` is the daemon's OWN one-project
//! binding, not a multi-project registry), so this is a fresh, best-effort
//! filesystem scan, deliberately narrow in scope (see `MAX_ENTRIES`).
//!
//! **Discovery heuristic.** This workspace's own convention — visible in the
//! very checkout this crate lives in — is `~/trusty-mpm-projects/<owner>/
//! <repo>` (an owner-scoped project root trusty-mpm's own tooling
//! established). When that directory exists, [`list_projects`] scans it two
//! levels deep (owner dirs, then their repo dirs) and returns every repo dir
//! that [`super::is_git_repo`] accepts — the same discriminator 7a's picker
//! already uses. When it does NOT exist (a machine without trusty-mpm's
//! project layout), this falls back to a single-level scan of the home
//! directory itself, on the theory that a developer machine without the
//! owner-scoped layout more commonly keeps repos as direct children of `~`
//! (e.g. `~/acme-api`) — still filtered to git repos only, so a home full of
//! Documents/Downloads/etc. does not flood the picker with noise.
//!
//! What: [`ProjectCandidate`] is one flat, pre-resolved row the picker can
//! render directly (no further filesystem calls needed, unlike 7a's
//! navigate-then-list flow); [`ProjectRoster`] is the whole response body.
//! [`list_projects`] is a thin `dirs::home_dir()`-resolving wrapper around
//! [`list_projects_in`], the actual (testable) scan — mirroring
//! `crate::serve::mod::build_router`/`build_router_at`'s real-vs-injectable
//! split so tests never touch the real developer home directory. Every
//! filesystem read here is best-effort: an unreadable or missing scan root
//! degrades to an empty (or partial) roster, never an error — mirroring
//! [`super::list_dir`]'s own per-entry best-effort philosophy, appropriate
//! here since this endpoint is a convenience shortlist, not the only way to
//! bind a project (an operator can still fall back to the [`super::list_dir`]
//! browse surface, which this ticket leaves untouched).
//! Test: `tests::*`.

use std::path::Path;

use serde::Serialize;

use super::is_git_repo;

/// Hard cap on the number of roster entries returned.
///
/// Why: this is a "quick pick" shortlist for a modal, not a paginated
/// browser — an unbounded scan of a large `trusty-mpm-projects` tree (or, in
/// the fallback case, a large home directory) must never turn a "small
/// roster" call into a slow, huge response. 200 comfortably covers a
/// realistic multi-owner project tree while staying obviously "small" per
/// the issue's own requirement.
/// Test: `tests::scan_is_capped_at_max_entries`.
const MAX_ENTRIES: usize = 200;

/// One candidate project row — everything the picker needs to render and
/// bind it without a further round-trip.
///
/// Why: mirrors [`super::DirEntryInfo`] in spirit (name + path a client can
/// bind directly), but flatter — no `is_dir` (every entry here is already
/// known to be a directory) and no `is_git_repo` (every entry here is
/// already known to be a git repo — filtering, not badging, is this
/// endpoint's job). `owner` carries the `trusty-mpm-projects/<owner>/<repo>`
/// layout's owner segment when the scan found it that way, so the picker can
/// disambiguate two repos that share a name under different owners; `None`
/// on the home-directory fallback path, where there is no owner segment.
/// Test: `tests::trusty_mpm_projects_layout_is_scanned_two_levels_deep`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProjectCandidate {
    /// The repo directory's own name (e.g. `trusty-tools`).
    pub name: String,
    /// Absolute path to the repo directory.
    pub path: String,
    /// The owning directory's name under `~/trusty-mpm-projects`, when the
    /// scan found this candidate that way; `None` on the home-directory
    /// fallback path.
    pub owner: Option<String>,
}

/// The `fs.list_projects` response body — everything the picker needs for
/// one render.
///
/// Why: a bare `Vec<ProjectCandidate>` would work equally well over the
/// wire, but wrapping it in a named struct (mirroring [`super::DirListing`])
/// leaves room to add roster-level metadata later (e.g. a `truncated: bool`
/// flag) without a breaking wire-shape change.
/// Test: `tests::empty_home_returns_empty_roster`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, Default)]
pub struct ProjectRoster {
    /// Candidate projects, sorted case-insensitively by `name`, capped at
    /// [`MAX_ENTRIES`].
    pub entries: Vec<ProjectCandidate>,
}

/// `fs.list_projects()` — the daemon-side entry point (issue #3365).
///
/// Why: the one function `protocol::list_projects` calls; resolves the real
/// home directory so the RPC handler itself stays a one-liner.
/// What: `dirs::home_dir()` unresolvable -> an empty [`ProjectRoster`]
/// (never an error — see module docs on the best-effort philosophy).
/// Otherwise delegates to [`list_projects_in`].
/// Test: exercised indirectly via `protocol::tests::list_projects_returns_entries_array`
/// (this function's own behaviour beyond delegation is untestable without
/// touching the real home directory; [`list_projects_in`] carries the real
/// coverage).
pub fn list_projects() -> ProjectRoster {
    match dirs::home_dir() {
        Some(home) => list_projects_in(&home),
        None => ProjectRoster::default(),
    }
}

/// The testable core of [`list_projects`]: scan a given `home` directory
/// rather than resolving the real one.
///
/// Why: split out so tests can point this at a tempdir instead of the
/// developer's (or CI runner's) actual home — the same seam
/// `serve::mod::build_router_at` uses for the workstream store's data dir.
/// What: if `<home>/trusty-mpm-projects` is a directory, scans it two levels
/// deep (owner dirs -> their repo dirs, each filtered to
/// [`super::is_git_repo`]) via [`scan_owner_layout`]; otherwise scans `home`
/// itself one level deep via [`scan_flat_layout`]. Either way, results are
/// sorted case-insensitively by `name` and truncated to [`MAX_ENTRIES`].
/// Test: `tests::trusty_mpm_projects_layout_is_scanned_two_levels_deep`,
/// `tests::falls_back_to_flat_home_scan_when_no_trusty_mpm_projects_dir`,
/// `tests::non_git_directories_are_excluded`,
/// `tests::scan_is_capped_at_max_entries`,
/// `tests::empty_home_returns_empty_roster`.
fn list_projects_in(home: &Path) -> ProjectRoster {
    let mpm_root = home.join("trusty-mpm-projects");
    let mut entries = if mpm_root.is_dir() {
        scan_owner_layout(&mpm_root)
    } else {
        scan_flat_layout(home)
    };
    entries.sort_by_key(|e| e.name.to_lowercase());
    entries.truncate(MAX_ENTRIES);
    ProjectRoster { entries }
}

/// Scan `<home>/trusty-mpm-projects/<owner>/<repo>` two levels deep,
/// keeping only `<repo>` dirs that are git repos.
///
/// Why: this workspace's own owner-scoped layout (see module docs) —
/// factored out from [`list_projects_in`] so each layout has its own,
/// independently testable scan function.
/// What: skips unreadable roots/owner dirs (best-effort — a permission
/// refusal on one owner dir must not blank the whole roster) and dotfile
/// entries at both levels.
/// Test: `tests::trusty_mpm_projects_layout_is_scanned_two_levels_deep`,
/// `tests::unreadable_owner_dir_is_skipped_not_fatal`.
fn scan_owner_layout(mpm_root: &Path) -> Vec<ProjectCandidate> {
    let mut out = Vec::new();
    let Ok(owners) = std::fs::read_dir(mpm_root) else {
        return out;
    };
    for owner_entry in owners.flatten() {
        let owner_path = owner_entry.path();
        if !owner_path.is_dir() {
            continue;
        }
        let owner_name = owner_entry.file_name().to_string_lossy().into_owned();
        if owner_name.starts_with('.') {
            continue;
        }
        let Ok(repos) = std::fs::read_dir(&owner_path) else {
            continue;
        };
        for repo_entry in repos.flatten() {
            let repo_path = repo_entry.path();
            if !repo_path.is_dir() {
                continue;
            }
            let repo_name = repo_entry.file_name().to_string_lossy().into_owned();
            if repo_name.starts_with('.') || !is_git_repo(&repo_path) {
                continue;
            }
            out.push(ProjectCandidate {
                name: repo_name,
                path: repo_path.display().to_string(),
                owner: Some(owner_name.clone()),
            });
        }
    }
    out
}

/// Scan `home` itself one level deep, keeping only entries that are git
/// repos.
///
/// Why: the fallback layout for a machine without `~/trusty-mpm-projects`
/// (see module docs) — factored out for the same reason as
/// [`scan_owner_layout`].
/// What: skips an unreadable `home` (best-effort -> empty) and dotfile
/// entries.
/// Test: `tests::falls_back_to_flat_home_scan_when_no_trusty_mpm_projects_dir`.
fn scan_flat_layout(home: &Path) -> Vec<ProjectCandidate> {
    let mut out = Vec::new();
    let Ok(read) = std::fs::read_dir(home) else {
        return out;
    };
    for entry in read.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || !is_git_repo(&path) {
            continue;
        }
        out.push(ProjectCandidate {
            name,
            path: path.display().to_string(),
            owner: None,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mkdir(p: &Path) {
        std::fs::create_dir_all(p).expect("mkdir");
    }

    fn mk_git_repo(p: &Path) {
        mkdir(p);
        mkdir(&p.join(".git"));
    }

    /// The owner-scoped `trusty-mpm-projects/<owner>/<repo>` layout must be
    /// scanned two levels deep, returning only git repos with their owner
    /// attached.
    #[test]
    fn trusty_mpm_projects_layout_is_scanned_two_levels_deep() {
        let home = tempfile::tempdir().expect("tempdir");
        let root = home.path().join("trusty-mpm-projects");
        mk_git_repo(&root.join("bobmatnyc").join("trusty-tools"));
        mkdir(&root.join("bobmatnyc").join("scratch")); // not a git repo
        mk_git_repo(&root.join("acme").join("widgets"));

        let roster = list_projects_in(home.path());

        assert_eq!(roster.entries.len(), 2);
        let names: Vec<&str> = roster.entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"trusty-tools"));
        assert!(names.contains(&"widgets"));
        let tt = roster
            .entries
            .iter()
            .find(|e| e.name == "trusty-tools")
            .expect("trusty-tools entry");
        assert_eq!(tt.owner.as_deref(), Some("bobmatnyc"));
    }

    /// Without a `trusty-mpm-projects` root, the scan falls back to `home`
    /// itself, one level deep, git repos only, `owner: None`.
    #[test]
    fn falls_back_to_flat_home_scan_when_no_trusty_mpm_projects_dir() {
        let home = tempfile::tempdir().expect("tempdir");
        mk_git_repo(&home.path().join("acme-api"));
        mkdir(&home.path().join("Downloads")); // not a git repo

        let roster = list_projects_in(home.path());

        assert_eq!(roster.entries.len(), 1);
        assert_eq!(roster.entries[0].name, "acme-api");
        assert_eq!(roster.entries[0].owner, None);
    }

    /// A non-git directory, at either scan level, must never appear in the
    /// roster — this is a project shortlist, not a general directory
    /// listing (unlike `super::list_dir`, which lists everything).
    #[test]
    fn non_git_directories_are_excluded() {
        let home = tempfile::tempdir().expect("tempdir");
        let root = home.path().join("trusty-mpm-projects");
        mkdir(&root.join("bobmatnyc").join("not-a-repo"));

        let roster = list_projects_in(home.path());

        assert!(roster.entries.is_empty());
    }

    /// A missing/unreadable `trusty-mpm-projects` OWNER dir must not blank
    /// the whole roster — best-effort per-owner scanning, mirroring
    /// `list_dir`'s per-entry philosophy.
    #[test]
    fn unreadable_owner_dir_is_skipped_not_fatal() {
        let home = tempfile::tempdir().expect("tempdir");
        let root = home.path().join("trusty-mpm-projects");
        // A file named like an owner dir (not a directory) is simply
        // skipped by the `owner_path.is_dir()` guard rather than erroring.
        mkdir(&root);
        std::fs::write(root.join("not-a-dir-owner"), "x").expect("write");
        mk_git_repo(&root.join("realowner").join("repo"));

        let roster = list_projects_in(home.path());

        assert_eq!(roster.entries.len(), 1);
        assert_eq!(roster.entries[0].owner.as_deref(), Some("realowner"));
    }

    /// A home with no `trusty-mpm-projects` dir and no git repos of its own
    /// returns an empty roster, not an error.
    #[test]
    fn empty_home_returns_empty_roster() {
        let home = tempfile::tempdir().expect("tempdir");
        mkdir(&home.path().join("Documents"));

        let roster = list_projects_in(home.path());

        assert!(roster.entries.is_empty());
    }

    /// A scan producing more than [`MAX_ENTRIES`] candidates must be capped,
    /// never returned in full — this is a "quick pick" shortlist, not a
    /// paginated browser (see `MAX_ENTRIES`'s own docs).
    #[test]
    fn scan_is_capped_at_max_entries() {
        let home = tempfile::tempdir().expect("tempdir");
        let owner = home.path().join("trusty-mpm-projects").join("owner");
        for i in 0..(MAX_ENTRIES + 25) {
            mk_git_repo(&owner.join(format!("repo-{i:04}")));
        }

        let roster = list_projects_in(home.path());

        assert_eq!(roster.entries.len(), MAX_ENTRIES);
    }

    /// Results must be sorted case-insensitively by name, regardless of
    /// owner or directory read order.
    #[test]
    fn results_are_sorted_case_insensitively_by_name() {
        let home = tempfile::tempdir().expect("tempdir");
        let root = home.path().join("trusty-mpm-projects");
        mk_git_repo(&root.join("owner").join("Zebra"));
        mk_git_repo(&root.join("owner").join("apple"));
        mk_git_repo(&root.join("owner").join("Banana"));

        let roster = list_projects_in(home.path());

        let names: Vec<&str> = roster.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["apple", "Banana", "Zebra"]);
    }
}
