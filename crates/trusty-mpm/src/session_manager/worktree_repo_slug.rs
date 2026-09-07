//! WHICH GitHub repository a worktree's pull-request lookups belong to (#7057).
//!
//! Why: every `gh` call on the reclaim and worktree-removal paths used to leave
//! the repository UNSTATED and let `gh` infer it from the working directory.
//! That inference is ambient — it depends on which remote `gh` picks, on a
//! `remote.<name>.gh-resolved` config key, and, when the directory git does not
//! root a repository at, on whatever enclosing repository a walk up the
//! filesystem finds. On 2026-09-07 a `tm session prune-worktrees --merged-prs`
//! run for `1m-consulting/adaptive-crm` resolved against
//! `hotstats/hotstats-product-poc` — a different registered project on the same
//! machine — and the merged-PR gate then reported "no pull request found" for
//! branches whose pull requests had just merged. Nothing in the argv named a
//! repository, so nothing in the output could disclose the substitution either.
//!
//! What: [`repo_slug_for`] derives `owner/repo` from the TARGET directory's own
//! `origin` remote, falling back to the checkout that owns it only when the
//! directory carries no `origin` of its own. [`parse_repo_slug`] is the pure
//! URL half. Both fail CLOSED: a missing or unparseable remote is an `Err`
//! naming the directory and the URL, never a guess and never another
//! repository — the [ADR-0045] absent-vs-undeterminable rule, applied to a gate
//! whose ALLOW deletes a checkout.
//!
//! [ADR-0045]: ../../../../docs/adr/0045-distinguish-absent-from-undeterminable-on-destructive-paths.md
//!
//! Test: `worktree_repo_slug_tests`.

use std::path::Path;

use super::worktree_registry::registry_root_for;
use super::worktree_safety::git_stdout;

/// URL schemes whose `owner/repo` tail this module trusts.
///
/// Why: the tail of a filesystem path also looks like `owner/repo`
/// (`/tmp/fixtures/remote.git` would read as `fixtures/remote`), so a remote
/// that is not a network URL must yield nothing rather than a plausible-looking
/// slug. `file://` is deliberately absent for the same reason.
/// Test: `a_local_path_remote_names_no_repository`.
const REMOTE_URL_SCHEMES: &[&str] = &["https://", "http://", "ssh://", "git://"];

/// The `owner/repo` a git remote URL names, or `None` when it names none.
///
/// Why: `gh --repo` takes `owner/repo`, and the two spellings a GitHub remote
/// arrives in — scp-like (`git@github.com:owner/repo.git`) and URL
/// (`https://github.com/owner/repo.git`) — reach it by different routes. Both
/// are parsed here so no call site grows its own half-parser.
/// What: strips a recognised scheme and its authority, or the scp-like
/// `[user@]host:` prefix, then takes the last two non-empty path segments with
/// any `.git` suffix removed. Anything else — a filesystem path, a `file://`
/// URL, a single-segment path, a segment carrying whitespace — is `None`.
/// Test: `https_remote_yields_its_owner_and_repo`,
/// `ssh_remote_yields_its_owner_and_repo`,
/// `scp_like_remote_yields_its_owner_and_repo`,
/// `a_local_path_remote_names_no_repository`,
/// `a_file_url_remote_names_no_repository`.
pub(crate) fn parse_repo_slug(url: &str) -> Option<String> {
    let url = url.trim();
    let path = match REMOTE_URL_SCHEMES.iter().find(|s| url.starts_with(**s)) {
        // `<scheme>://<authority>/<path>` — the authority carries a host and an
        // optional port and userinfo, none of which name the repository.
        Some(scheme) => url[scheme.len()..].split_once('/').map(|(_, p)| p)?,
        None => {
            // scp-like `[user@]host:owner/repo`. The colon must come before any
            // slash, or this is a filesystem path with a colon in it; and the
            // remainder must be relative, or this is a `<scheme>://` URL whose
            // scheme this module does not trust.
            let (authority, path) = url.split_once(':')?;
            if authority.is_empty() || authority.contains('/') || path.starts_with('/') {
                return None;
            }
            path
        }
    };
    slug_from_path(path)
}

/// The last two path segments of `path` as `owner/repo`.
fn slug_from_path(path: &str) -> Option<String> {
    let trimmed = path.trim_matches('/');
    let trimmed = trimmed.strip_suffix(".git").unwrap_or(trimmed);
    let mut segments = trimmed.rsplit('/').filter(|s| !s.is_empty());
    let repo = segments.next()?;
    let owner = segments.next()?;
    if repo.chars().any(char::is_whitespace) || owner.chars().any(char::is_whitespace) {
        return None;
    }
    Some(format!("{owner}/{repo}"))
}

/// The `owner/repo` every `gh` call rooted at `dir` must be pinned to (#7057).
///
/// Why: see the module doc. The repository is a property of the DIRECTORY under
/// inspection, so it is read there rather than inherited from the caller, the
/// daemon's working directory, or another registered project.
/// What: `git -C <dir> config --get remote.origin.url` (through
/// [`git_stdout`], which strips the environment able to point git at a
/// different repository), parsed by [`parse_repo_slug`]. A directory with no
/// `origin` of its own falls back to the checkout git names as the owner of its
/// worktree registry — and only there. Every other outcome is an `Err` whose
/// text names the directory and what was read, so a caller can print it.
/// Test: `two_worktrees_with_different_origins_resolve_to_different_repos`,
/// `a_directory_with_no_origin_falls_back_to_its_owning_checkout`,
/// `a_non_repository_directory_resolves_to_no_repository`.
pub(crate) fn repo_slug_for(dir: &Path) -> Result<String, String> {
    match origin_url(dir) {
        Some(url) => parse_repo_slug(&url).ok_or_else(|| unparseable(dir, &url)),
        None => slug_from_owning_checkout(dir),
    }
}

/// `dir`'s own `origin` URL, or `None` when git reports none.
fn origin_url(dir: &Path) -> Option<String> {
    git_stdout(dir, &["config", "--get", "remote.origin.url"])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// The slug of the checkout that owns `dir`'s worktree registry.
///
/// The fallback is ONE hop and never recurses: a checkout that is its own
/// registry root and still has no `origin` has nothing left to fall back to.
fn slug_from_owning_checkout(dir: &Path) -> Result<String, String> {
    let root = registry_root_for(dir).ok_or_else(|| missing(dir))?;
    if same_directory(&root, dir) {
        return Err(missing(dir));
    }
    let url = origin_url(&root).ok_or_else(|| missing(dir))?;
    parse_repo_slug(&url).ok_or_else(|| unparseable(&root, &url))
}

/// Are these two paths the same directory, symlinks resolved?
fn same_directory(a: &Path, b: &Path) -> bool {
    let resolve = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    resolve(a) == resolve(b)
}

/// The refusal for a directory whose repository cannot be established at all.
fn missing(dir: &Path) -> String {
    format!(
        "no `origin` remote at {} and no owning checkout carries one — the GitHub \
         repository to ask about cannot be established, so the lookup is refused \
         rather than aimed at a guess (#7057)",
        dir.display()
    )
}

/// The refusal for a remote URL that names no `owner/repo`.
fn unparseable(dir: &Path, url: &str) -> String {
    format!(
        "the `origin` remote at {} is {url:?}, which names no GitHub `owner/repo` — \
         the lookup is refused rather than aimed at a guess (#7057)",
        dir.display()
    )
}

#[cfg(test)]
#[path = "worktree_repo_slug_tests.rs"]
mod worktree_repo_slug_tests;
