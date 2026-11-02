//! Project key assignment and the registry-to-index-id link (#10530).
//!
//! Why: the trusty-search index id must derive from a value stored once in the
//! registry record, not from the checkout path or live git state, so `mv`, a
//! repo rename and `git remote remove` cannot move it. See the index-hygiene
//! design brief, section 1. The KEY is stable now; index NAMING keeps the legacy
//! basename until trusty-code can look the key up too (`REGISTRY_IDS_ADOPTED`).
//! What: [`project_key_from_repo_url`] and [`assign_project_key`] mint the key
//! the store writes at first register, reading a path-spelled `repo_url` through
//! [`local_repo_path`] (#10604); [`registered_project_for_root`] finds the
//! registry project whose checkout is a given root; [`index_id_for_registered_root`]
//! gives the one index id every caller (session launch, the residency producer,
//! `tm doctor`) uses for it.
//! Test: `project_key_tests.rs`, `store_tests.rs`.

use std::path::{Path, PathBuf};

use trusty_common::RegisteredProject;
use trusty_common::github_path::{UNKNOWN_OWNER, derive_github_path, parse_github_path};

use super::record::Project;

/// The local directory a path-spelled `repo_url` names, if it is one (#10604).
///
/// Why: a flat or hand-cloned checkout is often registered as `~/…` (a
/// `config.yaml` `projects:` entry is never shell-expanded). Parsed as a URL,
/// `~/trusty-mpm-projects/trusty-tools` dropped `~` as a host and named the
/// project `trusty-mpm-projects/trusty-tools`: a wrong key and a clone path
/// nested inside the projects root.
/// What: `Some(path)` for an absolute path, `Some(home)` for `~`, and
/// `Some(home/rest)` for `~/rest`. `None` for anything else — a URL — and for a
/// `~` spelling with no `home` to expand it against; [`is_path_spelling`] tells
/// those two apart.
/// Test: `a_flat_checkout_spelled_with_a_tilde_keys_by_its_origin_10604`,
/// `local_checkout_for_expands_a_tilde_repo_url_10604`.
pub fn local_repo_path(repo_url: &str, home: Option<&Path>) -> Option<PathBuf> {
    let trimmed = repo_url.trim();
    if trimmed.starts_with('/') {
        return Some(PathBuf::from(trimmed));
    }
    if trimmed == "~" {
        return home.map(Path::to_path_buf);
    }
    let rest = trimmed.strip_prefix("~/")?;
    home.map(|h| h.join(rest))
}

/// `true` when `repo_url` is spelled as a local path (`/…`, `~`, `~/…`, `~user/…`).
///
/// Why: such a value must never fall through to URL parsing (#10604).
pub fn is_path_spelling(repo_url: &str) -> bool {
    let trimmed = repo_url.trim();
    trimmed.starts_with('/') || trimmed.starts_with('~')
}

/// Canonical lowercase `owner/repo` for a repo URL or checkout path, if it has one.
///
/// Why: the key for a project with an origin is its `owner/repo`, matching the
/// group key the project map already uses.
/// What: [`project_key_from_repo_url_in`] against the process home directory.
/// Test: `key_from_url_forms_is_one_owner_repo`, `key_from_path_without_origin_is_none`.
pub fn project_key_from_repo_url(repo_url: &str) -> Option<String> {
    project_key_from_repo_url_in(repo_url, dirs::home_dir().as_deref())
}

/// [`project_key_from_repo_url`] with an explicit home directory.
///
/// What: parses a URL with [`parse_github_path`]; for a path spelling
/// ([`local_repo_path`], `~` expanded against `home`) reads the checkout's
/// `remote.origin.url` via [`derive_github_path`]. `None` when no owner can be
/// named (a path with no origin or an unexpandable `~`, a one-segment URL).
/// Test: `a_flat_checkout_spelled_with_a_tilde_keys_by_its_origin_10604`.
pub fn project_key_from_repo_url_in(repo_url: &str, home: Option<&Path>) -> Option<String> {
    let trimmed = repo_url.trim();
    // #10604: a `~` path is a checkout, never a URL whose host is `~`.
    let gh = if is_path_spelling(trimmed) {
        derive_github_path(&local_repo_path(trimmed, home)?)?
    } else {
        parse_github_path(trimmed)?
    };
    if gh.owner == UNKNOWN_OWNER || gh.owner.is_empty() || gh.repo.is_empty() {
        return None;
    }
    Some(format!(
        "{}/{}",
        gh.owner.to_ascii_lowercase(),
        gh.repo.to_ascii_lowercase()
    ))
}

/// Mint a project key: `owner/repo` when an origin exists, else `local:<uuid>`.
///
/// Why: every registered project needs a key; a project with no origin still
/// needs a stable one.
/// What: [`project_key_from_repo_url`], falling back to a fresh `local:<uuid>`.
/// Test: `key_from_path_without_origin_is_none`.
pub fn assign_project_key(repo_url: &str) -> String {
    project_key_from_repo_url(repo_url).unwrap_or_else(|| format!("local:{}", uuid::Uuid::new_v4()))
}

/// The registry project whose local checkout is `root`.
///
/// Why: registration, the residency producer and the doctor each hold a root and
/// need the owning project's `(name, key)` to derive the id. Matching on the
/// checkout path only keeps a linked worktree or a session clone from claiming
/// the project's shared index.
/// What: the first project with a stored key whose
/// [`local_checkout_for`](super::resolver::local_checkout_for) names the same
/// directory as `root` ([`trusty_common::identifies_same_path`]).
/// Test: `registered_project_for_root_matches_the_checkout_only`.
pub fn registered_project_for_root(projects: &[Project], root: &Path) -> Option<RegisteredProject> {
    projects.iter().find_map(|p| {
        let key = p.project_key.as_deref()?;
        let checkout = super::resolver::local_checkout_for(p)?;
        trusty_common::identifies_same_path(&checkout, root)
            .then(|| RegisteredProject::new(p.name.clone(), key))
    })
}

/// [`registered_project_for_root`] against the on-disk registry.
///
/// Why: session launch and the doctor run without a registry handle.
/// What: reads `<registry dir>/projects.json` with no lock (every publish is an
/// atomic rename); `None` when it is absent or unreadable, so the caller falls
/// back to the legacy basename id.
/// Test: `index_id_matches_between_registration_publication_and_trusty_code`.
pub fn registered_project_for_root_in(
    registry_dir: &Path,
    root: &Path,
) -> Option<RegisteredProject> {
    let projects = super::store::read_projects_blocking(registry_dir)?;
    registered_project_for_root(&projects, root)
}

/// The one index id for `root`: registry-derived when registered, else legacy.
///
/// Why: the id tm publishes must equal the id registration creates.
/// What: resolves `root` to its project root exactly as
/// `ensure_project_indexed_reporting_as` does, then applies
/// [`trusty_common::index_id_for_root`] over [`registered_project_for_root_in`].
/// Test: `index_id_matches_between_registration_publication_and_trusty_code`.
pub fn index_id_for_root_in(registry_dir: &Path, root: &Path) -> String {
    let root = trusty_common::resolve_project_root(root);
    let registered = registered_project_for_root_in(registry_dir, &root);
    trusty_common::index_id_for_root(&root, registered.as_ref())
}

/// [`index_id_for_root_in`] against the daemon's registry directory.
///
/// Why: `tm doctor` has a root and no registry handle.
/// Test: `index_id_matches_between_registration_publication_and_trusty_code`.
pub fn index_id_for_registered_root(root: &Path) -> String {
    index_id_for_root_in(&super::registry_data_dir(), root)
}

/// Every index id accepted as `root`'s index until naming adopts the registry id.
///
/// Why: the doctor must report healthy for the legacy basename and the registry
/// id alike (#10530).
/// What: the current naming id first, then the registry id and the legacy
/// basename, deduplicated.
/// Test: `accepted_ids_include_legacy_and_registry_forms` (doctor_tests.rs).
pub fn accepted_index_ids_for(root: &Path, registered: Option<&RegisteredProject>) -> Vec<String> {
    let mut ids = vec![trusty_common::index_id_for_root(root, registered)];
    ids.push(trusty_common::registry_id_for_root(root, registered));
    ids.push(trusty_common::derive_index_id(root));
    let mut seen = std::collections::HashSet::new();
    ids.retain(|id| !id.trim().is_empty() && seen.insert(id.clone()));
    ids
}

#[cfg(test)]
#[path = "project_key_tests.rs"]
mod project_key_tests;
