//! Register a session's own checkout in `projects.json`, and resolve it (#10368).
//!
//! Why: a session started in a checkout wrote only `project-paths.json`, so
//! `projects.json` had no row for it and `tm secrets assign` could name no
//! project. Owner ruling D50 (2026-10-24): owner- and user-scope secret reads
//! happen only from a tm-registered checkout, so registration must never reach
//! past the session's own project root.
//! What: [`register_session_checkout`] writes one path-form row for the git
//! root holding the session's directory (the daemon's
//! [`register_session_checkout_in`] stores a canonical managed clone by its
//! GitHub URL, #11828; a linked worktree writes its main checkout's row,
//! #11217), through the locked
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
        /// This session's canonical checkout root.
        root: PathBuf,
    },
    /// The directory is in no git checkout; nothing was written.
    NotACheckout,
}

/// Why [`register_session_checkout`] left the checkout unregistered.
#[derive(Debug, Error)]
pub enum SessionRegistrationError {
    /// The git root is the filesystem root, the home directory or above it (D50).
    #[error(
        "refusing to register {0} as a project: it is the home directory, above it, or the filesystem root"
    )]
    Unregistrable(PathBuf),
    /// Git could not confirm the directory holding `.git` as its own top level.
    #[error(
        "refusing to register {0} as a project: git does not confirm it as a checkout's top level"
    )]
    UnverifiedRoot(PathBuf),
    /// A linked worktree whose main working tree git cannot name (#11217).
    #[error(
        "refusing to register the linked worktree {0}: its main checkout could not be resolved (a bare repository, or a common dir with no working tree)"
    )]
    MainCheckoutUnresolved(PathBuf),
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
/// What: finds the git root above `cwd`'s canonical path; no root is
/// [`SessionRegistration::NotACheckout`], without a write. A linked worktree
/// registers its main checkout instead (#11217, [`main_checkout_of`]): the
/// main working tree named by `--git-common-dir` goes through every check
/// below as if the session had started there; a main checkout git cannot
/// name is [`SessionRegistrationError::MainCheckoutUnresolved`], and the
/// worktree path is never stored. A root that is `/`, `home` or an ancestor of `home` is
/// [`SessionRegistrationError::Unregistrable`] (D50). A root git does not
/// report as its own `--show-toplevel`, or whose git-dir check fails, is
/// [`SessionRegistrationError::UnverifiedRoot`]. A row already present (same
/// name, path or key) is found by a plain read and nothing is written;
/// otherwise one `register_unless_present` call stores a row whose `repo_url`
/// is the canonical root and whose name is its basename. A kept row with the
/// same key or path is [`SessionRegistration::AlreadyRegistered`], carrying
/// both checkouts; one that matched on name only is
/// [`SessionRegistrationError::NameTaken`]. A kept row is never rewritten.
/// Test: `session_start_registers_the_checkout_and_assign_resolves_it`,
/// `session_start_registration_is_idempotent`,
/// `session_start_registration_failure_is_an_error_not_a_registration`,
/// `session_start_never_registers_home_or_its_ancestors`,
/// `session_start_refuses_a_root_git_does_not_confirm`,
/// `session_start_second_start_leaves_the_bytes_alone`,
/// `session_start_through_a_symlink_to_home_is_refused`,
/// `session_start_in_an_old_tilde_record_checkout_is_the_same_repo_10604`,
/// `session_start_name_collision_with_another_repo_is_name_taken_10604`,
/// `client_registration_keeps_the_path_form_at_the_canonical_path_11828`,
/// `a_linked_worktree_session_registers_the_main_checkout_11217`,
/// `a_worktree_whose_main_checkout_cannot_be_resolved_writes_nothing_11217`,
/// `a_bare_repo_worktree_writes_nothing_11217`,
/// `a_worktree_of_repo_a_never_resolves_to_repo_b_11217`.
pub async fn register_session_checkout(
    registry_dir: &Path,
    cwd: &Path,
    home: Option<&Path>,
) -> Result<SessionRegistration, SessionRegistrationError> {
    // #11828: the client resolves its own repos root, so it never claims a URL.
    register_checkout(registry_dir, cwd, home, None).await
}

/// [`register_session_checkout`] for the daemon: a canonical clone registers
/// by its GitHub URL.
///
/// Why: #11828 — a path `repo_url` names no owner (the TUI lists it under
/// `(local)`) and never matches a managed session's URL, so the row a `tm run`
/// clone got showed 0 sessions.
/// What: as [`register_session_checkout`], except the stored `repo_url` is
/// `https://github.com/<owner>/<repo>` when the root's origin parses, its host
/// is `github.com`, and the root is the same directory as
/// `<repos_root>/<owner>/<repo>` ([`base_clone_path_in`](crate::core::repo_paths::base_clone_path_in)).
/// Owner and repo keep the origin's spelling. Any other checkout keeps the
/// path form, and so does every checkout when `repos_root` is `None` (the
/// daemon could not resolve it).
/// Test: `daemon_registers_a_canonical_clone_by_its_github_url_11828`,
/// `daemon_keeps_the_path_form_off_the_canonical_clone_11828`,
/// `daemon_registers_by_path_on_a_broken_install_file_11828` (env_serial).
pub async fn register_session_checkout_in(
    registry_dir: &Path,
    cwd: &Path,
    home: Option<&Path>,
    repos_root: Option<&Path>,
) -> Result<SessionRegistration, SessionRegistrationError> {
    register_checkout(registry_dir, cwd, home, repos_root).await
}

/// Both entry points' work; `repos_root` enables the #11828 URL form.
async fn register_checkout(
    registry_dir: &Path,
    cwd: &Path,
    home: Option<&Path>,
    repos_root: Option<&Path>,
) -> Result<SessionRegistration, SessionRegistrationError> {
    use crate::core::project_aliases::find_git_root;
    // #10368: walk the real path, so a symlink cannot hide home or a parent.
    let cwd = cwd.canonicalize().map_err(ProjectStoreError::Io)?;
    let Some(root) = find_git_root(&cwd) else {
        return Ok(SessionRegistration::NotACheckout);
    };
    let root = root.canonicalize().map_err(ProjectStoreError::Io)?;
    let root = match verified_root(&root, home)? {
        Checkout::Main => root,
        // #11217: owner ruling — a linked worktree registers its main
        // checkout, which must pass the same D50 and git checks itself.
        Checkout::LinkedWorktree => {
            let main = main_checkout_of(&root)
                .ok_or_else(|| SessionRegistrationError::MainCheckoutUnresolved(root.clone()))?;
            match verified_root(&main, home)? {
                Checkout::Main => main,
                // #11217: a "main" that is itself a worktree is not resolved.
                Checkout::LinkedWorktree => {
                    return Err(SessionRegistrationError::MainCheckoutUnresolved(root));
                }
            }
        }
    };
    store_checkout(registry_dir, root, repos_root).await
}

/// Whether a verified root is a main checkout or a linked worktree.
enum Checkout {
    Main,
    LinkedWorktree,
}

/// The D50 and git checks one canonical `root` must pass before it is stored.
///
/// What: `/`, `home` or an ancestor of `home` is `Unregistrable`; a root git
/// does not report as its own `--show-toplevel`, or whose git-dir probe fails,
/// is `UnverifiedRoot`. Otherwise says whether `root` is a linked worktree.
fn verified_root(root: &Path, home: Option<&Path>) -> Result<Checkout, SessionRegistrationError> {
    // #10368: D50 — never the home directory, an ancestor of it, or `/`.
    let home = home.map(|h| h.canonicalize().unwrap_or_else(|_| h.to_path_buf()));
    if root.parent().is_none() || home.is_some_and(|h| h.starts_with(root)) {
        return Err(SessionRegistrationError::Unregistrable(root.to_path_buf()));
    }
    // #10368: a `.git` entry is not proof; git must name this root its top
    // level, and any git failure refuses.
    if git_toplevel(root).as_deref() != Some(root) {
        return Err(SessionRegistrationError::UnverifiedRoot(root.to_path_buf()));
    }
    match is_linked_worktree(root) {
        Some(false) => Ok(Checkout::Main),
        Some(true) => Ok(Checkout::LinkedWorktree),
        None => Err(SessionRegistrationError::UnverifiedRoot(root.to_path_buf())),
    }
}

/// The main working tree of the linked worktree `root`, or `None`.
///
/// Why: #11217 — a worktree shares its main checkout's repository, origin and
/// therefore its secret scopes, so the main checkout is its project row.
/// What: `git rev-parse --path-format=absolute --git-common-dir`, canonical;
/// the main tree is that directory's parent, and only when the parent's own
/// `.git` canonicalizes to the same common dir. A bare repository (whose
/// common dir has no working tree above it), a missing common dir, a git
/// failure or non-UTF-8 output is `None`: the caller refuses, never guesses.
/// Test: `a_linked_worktree_session_registers_the_main_checkout_11217`,
/// `a_bare_repo_worktree_writes_nothing_11217`,
/// `a_worktree_whose_main_checkout_cannot_be_resolved_writes_nothing_11217`.
fn main_checkout_of(root: &Path) -> Option<PathBuf> {
    // #11217: scrubbed env, so `GIT_COMMON_DIR` cannot name repo X.
    let common = git_stdout(
        root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let common = PathBuf::from(common.trim()).canonicalize().ok()?;
    let main = common.parent()?.to_path_buf();
    // #11217: fail closed — the parent must hold this very common dir.
    (main.join(".git").canonicalize().ok()? == common).then_some(main)
}

/// Store `root`, a verified main checkout, once (#10368, #11828).
async fn store_checkout(
    registry_dir: &Path,
    root: PathBuf,
    repos_root: Option<&Path>,
) -> Result<SessionRegistration, SessionRegistrationError> {
    // #11828: a canonical managed clone is stored by its GitHub URL.
    let repo_url = repos_root
        .and_then(|repos_root| canonical_clone_url(&root, repos_root))
        .unwrap_or_else(|| root.to_string_lossy().into_owned());
    let name = crate::core::project::name_from_path(&root);
    let project = session_project(&name, &repo_url);
    let key = super::project_key_from_repo_url(&repo_url);
    // #10368: a present row is answered from a plain read; the locked write
    // would republish the whole file on every start.
    let present = super::store::read_projects(registry_dir)?
        .into_iter()
        .find(|p| super::registry::holds_registration(p, &project, key.as_deref()));
    let kept = match present {
        Some(kept) => kept,
        None => {
            let registry = ProjectRegistry::load(registry_dir).await?;
            match registry.register_unless_present(project).await? {
                None => return Ok(SessionRegistration::Registered { name }),
                Some(kept) => kept,
            }
        }
    };
    // #10604: an old `~` row keeps its stale stored key; `held_keys` adds the
    // key its checkout's origin gives now. A failed lookup adds nothing.
    let same_repo = kept.repo_url == repo_url
        || key.as_deref().is_some_and(|key| {
            kept_key(&kept).as_deref() == Some(key)
                || super::registry::held_keys(&kept).iter().any(|k| k == key)
        });
    if !same_repo {
        return Err(SessionRegistrationError::NameTaken { name, root });
    }
    Ok(SessionRegistration::AlreadyRegistered {
        checkout: super::local_checkout_for(&kept),
        name: kept.name,
        root,
    })
}

/// `https://github.com/<owner>/<repo>` when `root` is the managed clone of its
/// own GitHub origin under `repos_root`, else `None` (#11828).
///
/// What: no origin, an origin that does not parse, a host other than
/// `github.com`, or a root that is not `<repos_root>/<owner>/<repo>` is `None`:
/// the path form, which is the right row for such a checkout, not a failure.
/// The URL is rebuilt from the parsed parts, never the raw origin (#9124).
fn canonical_clone_url(root: &Path, repos_root: &Path) -> Option<String> {
    let origin = trusty_common::github_path::derive_remote_repo(root).ok()?;
    if !origin.host.eq_ignore_ascii_case("github.com") {
        return None;
    }
    let clone =
        crate::core::repo_paths::base_clone_path_in(repos_root, &origin.owner, &origin.repo);
    trusty_common::identifies_same_path(root, &clone)
        .then(|| format!("https://github.com/{}/{}", origin.owner, origin.repo))
}

/// `git -C root <args>`'s stdout, or `None` on a spawn failure, a non-zero
/// exit or non-UTF-8 output.
///
/// Why: #11217 — an inherited `GIT_DIR`, `GIT_COMMON_DIR` or `GIT_WORK_TREE`
/// points git at another repository, so a worktree of repo A would resolve to
/// repo X and register X; the trusty-secrets server scrubs the same way (#9065).
/// What: runs through [`git_command`](crate::session_manager::worktree_safety::git_command),
/// which removes every repository-redirecting variable from the child env.
/// Test: `inherited_git_common_dir_never_registers_another_repo_11217` (env_serial).
fn git_stdout(root: &Path, args: &[&str]) -> Option<String> {
    let out = crate::session_manager::worktree_safety::git_command(root, args)
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    String::from_utf8(out.stdout).ok()
}

/// `git -C root rev-parse --show-toplevel`, canonicalized; `None` on any failure.
fn git_toplevel(root: &Path) -> Option<PathBuf> {
    // #11217: scrubbed env, so `GIT_WORK_TREE` cannot name another top level.
    let top = git_stdout(root, &["rev-parse", "--show-toplevel"])?;
    PathBuf::from(top.trim()).canonicalize().ok()
}

/// Whether `root` is a linked worktree: its git dir and common dir differ.
///
/// What: one scrubbed `rev-parse --path-format=absolute --git-dir
/// --git-common-dir`; anything but two lines, or any git failure, is `None`.
fn is_linked_worktree(root: &Path) -> Option<bool> {
    // #11217: replaces the unscrubbed `git_dir_differs_from_common` here.
    let out = git_stdout(
        root,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-dir",
            "--git-common-dir",
        ],
    )?;
    let mut lines = out.lines();
    let (git_dir, common) = (lines.next()?, lines.next()?);
    lines.next().is_none().then(|| git_dir != common)
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
