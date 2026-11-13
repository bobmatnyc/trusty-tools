//! Register a session's own checkout in `projects.json`, and resolve it (#10368).
//!
//! Why: a session started in a checkout wrote only `project-paths.json`, so
//! `projects.json` had no row for it and `tm secrets assign` could name no
//! project. Owner ruling D50 (2026-10-24): owner- and user-scope secret reads
//! happen only from a tm-registered checkout, so registration must never reach
//! past the session's own project root.
//! What: [`register_session_checkout`] writes one path-form row for the git
//! root holding the session's directory, through the locked
//! [`ProjectRegistry::register_unless_present`]; [`registered_checkout`] maps an
//! `owner/repo` key back to that checkout. Both fail closed: every store error
//! is returned, never read as "registered".
//! Test: `session_registration_tests.rs`.

use std::path::{Path, PathBuf};

use thiserror::Error;

use super::record::Project;
use super::registry::ProjectRegistry;
use super::store::ProjectStoreError;

/// What [`register_session_checkout`] did when it wrote nothing wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionRegistration {
    /// A new row named `name` now holds the checkout.
    Registered {
        /// The row's name.
        name: String,
    },
    /// A row for the same repository or path already existed; nothing was written.
    AlreadyRegistered {
        /// The kept row's name.
        name: String,
        /// The kept row's checkout, which may differ from this session's.
        checkout: Option<PathBuf>,
    },
    /// The directory is in no git checkout; nothing was written.
    NotACheckout,
    /// The checkout is a linked worktree; its main checkout is the project.
    LinkedWorktree,
}

/// Why [`register_session_checkout`] left the checkout unregistered.
#[derive(Debug, Error)]
pub enum SessionRegistrationError {
    /// The git root is the filesystem root, the home directory or above it (D50).
    #[error(
        "refusing to register {0} as a project: it is the home directory, above it, or the filesystem root"
    )]
    Unregistrable(PathBuf),
    /// Another repository's row already holds the checkout's name.
    #[error("project name {name} already belongs to another repository; {root} was not registered")]
    NameTaken {
        /// The name both rows want.
        name: String,
        /// The checkout left unregistered.
        root: PathBuf,
    },
    /// The registry could not be read or written.
    #[error(transparent)]
    Store(#[from] ProjectStoreError),
}

/// Register the git checkout holding `cwd` as a project, once.
///
/// Why: #10368 — a session started in a checkout must be a registered target
/// for `tm secrets` with no manual `tm project init`.
/// What: finds the git root above `cwd`; no root is [`SessionRegistration::NotACheckout`]
/// and a linked worktree is [`SessionRegistration::LinkedWorktree`], both
/// without a write. A root that is `/`, `home` or an ancestor of `home` is
/// [`SessionRegistrationError::Unregistrable`] (D50). Otherwise one
/// `register_unless_present` call stores a row whose `repo_url` is the
/// canonical root and whose name is its basename. A kept row with the same key
/// or path is [`SessionRegistration::AlreadyRegistered`]; a kept row that
/// matched on name only is [`SessionRegistrationError::NameTaken`]. A kept row
/// is never rewritten.
/// Test: `session_start_registers_the_checkout_and_assign_resolves_it`,
/// `session_start_registration_is_idempotent`,
/// `session_start_registration_failure_is_an_error_not_a_registration`,
/// `session_start_never_registers_home_or_its_ancestors`.
pub async fn register_session_checkout(
    registry_dir: &Path,
    cwd: &Path,
    home: Option<&Path>,
) -> Result<SessionRegistration, SessionRegistrationError> {
    use crate::core::project_aliases::{find_git_root, git_dir_differs_from_common};
    let Some(root) = find_git_root(cwd) else {
        return Ok(SessionRegistration::NotACheckout);
    };
    let root = root.canonicalize().map_err(ProjectStoreError::Io)?;
    // #10368: D50 — never the home directory, an ancestor of it, or `/`.
    let home = home.map(|h| h.canonicalize().unwrap_or_else(|_| h.to_path_buf()));
    if root.parent().is_none() || home.is_some_and(|h| h.starts_with(&root)) {
        return Err(SessionRegistrationError::Unregistrable(root));
    }
    if git_dir_differs_from_common(&root) == Some(true) {
        return Ok(SessionRegistration::LinkedWorktree);
    }
    let repo_url = root.to_string_lossy().into_owned();
    let name = crate::core::project::name_from_path(&root);
    let registry = ProjectRegistry::load(registry_dir).await?;
    let Some(kept) = registry
        .register_unless_present(session_project(&name, &repo_url))
        .await?
    else {
        return Ok(SessionRegistration::Registered { name });
    };
    let key = super::project_key_from_repo_url(&repo_url);
    let same_repo = kept.repo_url == repo_url || (key.is_some() && kept_key(&kept) == key);
    if !same_repo {
        return Err(SessionRegistrationError::NameTaken { name, root });
    }
    Ok(SessionRegistration::AlreadyRegistered {
        checkout: super::local_checkout_for(&kept),
        name: kept.name,
    })
}

/// The row a session start writes: identity fields only, every option unset.
fn session_project(name: &str, repo_url: &str) -> Project {
    Project {
        name: name.to_string(),
        repo_url: repo_url.to_string(),
        default_branch: "main".to_string(),
        stack_hint: None,
        tags: vec![],
        description: None,
        gh_user: None,
        gh_account: None,
        github: None,
        commit_name: None,
        commit_email: None,
        worktree: None,
        project_key: None,
        decommissioned_at: None,
        archived_at: None,
    }
}

/// A row's `owner/repo` key: the stored one, else derived from its `repo_url`.
fn kept_key(project: &Project) -> Option<String> {
    project
        .project_key
        .clone()
        .or_else(|| super::project_key_from_repo_url(&project.repo_url))
}

/// Why [`registered_checkout`] found no checkout.
#[derive(Debug, Error)]
pub enum CheckoutLookupError {
    /// No live row holds the key, or its checkout directory is gone.
    #[error("no registered checkout for {0}")]
    NotRegistered(String),
    /// Rows for the key name more than one checkout.
    #[error("more than one registered checkout for {0}")]
    Ambiguous(String),
    /// The registry could not be read.
    #[error(transparent)]
    Store(#[from] ProjectStoreError),
}

/// The one checkout directory the registry holds for `owner/repo`.
///
/// Why: #10368 — `tm secrets assign --project owner/repo` writes into the
/// registered checkout, never a guessed directory.
/// What: reads `<registry_dir>/projects.json` (absent is empty, unreadable or
/// corrupt is [`CheckoutLookupError::Store`]); keeps rows that are neither
/// decommissioned nor archived and whose key ([`kept_key`]) equals `key`
/// case-insensitively; maps each to [`super::local_checkout_for`] and keeps
/// the existing directories. None is `NotRegistered`, two different ones are
/// `Ambiguous`.
/// Test: `session_start_registers_the_checkout_and_assign_resolves_it`,
/// `session_start_registration_failure_is_an_error_not_a_registration`.
pub fn registered_checkout(registry_dir: &Path, key: &str) -> Result<PathBuf, CheckoutLookupError> {
    let key = key.to_ascii_lowercase();
    let mut found: Vec<PathBuf> = super::store::read_projects(registry_dir)?
        .iter()
        .filter(|p| !p.is_decommissioned() && !p.is_archived())
        .filter(|p| kept_key(p).as_deref() == Some(key.as_str()))
        .filter_map(super::local_checkout_for)
        .filter(|dir| dir.is_dir())
        .collect();
    found.sort();
    found.dedup();
    match found.len() {
        0 => Err(CheckoutLookupError::NotRegistered(key)),
        1 => Ok(found.remove(0)),
        _ => Err(CheckoutLookupError::Ambiguous(key)),
    }
}

#[cfg(test)]
#[path = "session_registration_tests.rs"]
mod tests;
