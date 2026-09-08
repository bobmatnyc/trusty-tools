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
//! The HOST is part of that answer. `gh --repo` takes `[HOST/]OWNER/REPO` and
//! resolves a bare `OWNER/REPO` against its own default host, so a slug built
//! from a GitHub Enterprise remote — or from any host a `GH_HOST` override
//! points at — that dropped the host asked github.com instead, and answered
//! from there if a repository with those two names existed. That is the same
//! wrong-repository substitution, one level up, and equally silent.
//!
//! An SSH `Host` ALIAS is not a host (#7196). A multi-account operator writes
//! `git@github-duetto:duettoresearch/APEX.git` and lets `~/.ssh/config` rewrite
//! `github-duetto` to `github.com`. Nothing but `ssh` performs that rewrite, so
//! the alias arrived here as the GitHub host and `gh --repo
//! github-duetto/duettoresearch/APEX` failed with "error connecting to
//! github-duetto" — four merged-PR worktrees refused at gate 5 on 2026-09-08,
//! and no repository behind such an alias could ever be reclaimed. An SSH
//! remote's host is now resolved through [`super::ssh_host_alias`] before it
//! becomes part of a slug.
//!
//! What: [`repo_slug_for`] derives `[host/]owner/repo` from the TARGET
//! directory's own `origin` remote, falling back to the checkout that owns it
//! only when the directory carries no `origin` of its own. [`parse_repo_slug`]
//! is the URL half, and is where the host is resolved and then kept or — for
//! [`DEFAULT_GH_HOST`], which `gh` assumes — omitted. Both fail CLOSED: a
//! missing remote, an unparseable one, and an SSH alias that resolves to
//! nothing are each an `Err` naming the directory and what was read, never a
//! guess and never another repository — the [ADR-0045]
//! absent-vs-undeterminable rule, applied to a gate whose ALLOW deletes a
//! checkout.
//!
//! [ADR-0045]: ../../../../docs/adr/0045-distinguish-absent-from-undeterminable-on-destructive-paths.md
//!
//! Test: `worktree_repo_slug_tests`.

use std::path::Path;

use super::ssh_host_alias::SshHostAliases;
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

/// The host `gh --repo` addresses when a slug names none (#7057).
///
/// Why: `gh` resolves a bare `OWNER/REPO` against its own default host, so only
/// a remote already on that host may drop it. Every other host has to survive
/// into the slug or the lookup silently changes which server it asks.
const DEFAULT_GH_HOST: &str = "github.com";

/// The SSH transports whose host `~/.ssh/config` may rename (#7196).
///
/// Why: only `ssh` consults that config, so only a remote git hands to `ssh`
/// can carry an alias. Resolving an `https://` host through it would rewrite a
/// host git never sends to `ssh` at all — a change of SERVER made on evidence
/// that does not apply.
const SSH_URL_SCHEME: &str = "ssh://";

/// Why a git remote URL names no repository this module will ask `gh` about.
///
/// Why: the two cases need different refusals. "This is a filesystem path"
/// tells the operator the remote is not a GitHub one at all; "`github-duetto`
/// resolves to nothing" tells them which `~/.ssh/config` entry is missing. One
/// `None` for both is what made #7196 read as a `gh` connection error.
/// What: carried by [`parse_repo_slug`] and rendered by [`refusal`].
/// Test: `an_unparseable_origin_refuses_and_quotes_the_url`,
/// `an_unresolvable_ssh_alias_refuses_and_names_the_alias`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SlugRefusal {
    /// The URL names no `owner/repo` — a filesystem path, a `file://` URL, a
    /// single-segment path, a segment carrying whitespace.
    NoRepository,
    /// An SSH host that neither `~/.ssh/config` renames nor looks like a
    /// machine name. The string is the alias, verbatim.
    UnresolvedSshAlias(String),
}

/// The `[host/]owner/repo` a git remote URL names.
///
/// Why: `gh --repo` takes `[HOST/]OWNER/REPO`, and the two spellings a GitHub
/// remote arrives in — scp-like (`git@github.com:owner/repo.git`) and URL
/// (`https://github.com/owner/repo.git`) — reach it by different routes. Both
/// are parsed here so no call site grows its own half-parser.
///
/// #7057: the HOST is part of the answer. A slug built from a GitHub Enterprise
/// remote used to arrive as bare `owner/repo`, which `gh` then resolved against
/// its default host — answering for a same-named repository there, with nothing
/// in the reply disclosing the substitution. That is the same wrong-repository
/// defect this module exists to close, one host up.
///
/// #7196: an SSH host is an ALIAS until `~/.ssh/config` says otherwise, so
/// `aliases` is consulted before the host is trusted — see [`resolve_ssh_host`].
/// What: strips a recognised scheme and its authority, or the scp-like
/// `[user@]host:` prefix, then takes the last two non-empty path segments with
/// any `.git` suffix removed. The authority's host — userinfo and port removed,
/// lowercased, and for an SSH transport resolved through `aliases` — is
/// prepended unless it is [`DEFAULT_GH_HOST`], which `gh` assumes.
/// Test: `https_remote_yields_its_owner_and_repo`,
/// `ssh_remote_yields_its_owner_and_repo`,
/// `scp_like_remote_yields_its_owner_and_repo`,
/// `a_non_default_host_survives_into_the_slug`,
/// `an_ssh_alias_resolves_to_the_host_it_names`,
/// `an_unresolvable_ssh_alias_refuses_and_names_the_alias`,
/// `an_enterprise_worktree_names_its_host_in_the_repo_flag`,
/// `a_local_path_remote_names_no_repository`,
/// `a_file_url_remote_names_no_repository`.
pub(crate) fn parse_repo_slug(url: &str, aliases: &SshHostAliases) -> Result<String, SlugRefusal> {
    let url = url.trim();
    let (authority, path, ssh) = match REMOTE_URL_SCHEMES.iter().find(|s| url.starts_with(**s)) {
        // `<scheme>://<authority>/<path>` — the authority carries the host plus
        // an optional port and userinfo, which `host_from_authority` drops.
        Some(scheme) => {
            let (authority, path) = url[scheme.len()..]
                .split_once('/')
                .ok_or(SlugRefusal::NoRepository)?;
            (authority, path, *scheme == SSH_URL_SCHEME)
        }
        None => {
            // scp-like `[user@]host:owner/repo`. The colon must come before any
            // slash, or this is a filesystem path with a colon in it; and the
            // remainder must be relative, or this is a `<scheme>://` URL whose
            // scheme this module does not trust.
            let (authority, path) = url.split_once(':').ok_or(SlugRefusal::NoRepository)?;
            if authority.is_empty() || authority.contains('/') || path.starts_with('/') {
                return Err(SlugRefusal::NoRepository);
            }
            // git hands this spelling straight to `ssh`, so it is the shape the
            // #7196 aliases arrive in.
            (authority, path, true)
        }
    };
    let host = host_from_authority(authority).ok_or(SlugRefusal::NoRepository)?;
    let slug = slug_from_path(path).ok_or(SlugRefusal::NoRepository)?;
    let host = if ssh {
        resolve_ssh_host(&host, aliases)?
    } else {
        host
    };
    // #7057: a non-default host must reach `gh --repo`, or the lookup asks the
    // wrong server.
    if host == DEFAULT_GH_HOST {
        Ok(slug)
    } else {
        Ok(format!("{host}/{slug}"))
    }
}

/// The machine an SSH remote's host names, resolving a `~/.ssh/config` alias.
///
/// Why: `gh --repo [HOST/]OWNER/REPO` addresses a SERVER, and an alias
/// addresses none — passing `github-duetto` through produced a slug `gh` could
/// only fail to connect to, which gate 5 then read as "the merged-PR lookup
/// failed" for a tree it should have reclaimed (#7196).
/// What: three outcomes, in order. A host `aliases` renames becomes the name it
/// gives. A host carrying a `.` is taken as a real machine, which is what keeps
/// the #7057 GitHub Enterprise case working when the operator declares no alias
/// for it. Anything else — a bare name nothing renames — is
/// [`SlugRefusal::UnresolvedSshAlias`], because a dotless name is not a
/// reachable host and guessing which server it meant is the substitution this
/// module exists to prevent.
/// Test: `an_ssh_alias_resolves_to_the_host_it_names`,
/// `an_unresolvable_ssh_alias_refuses_and_names_the_alias`,
/// `an_undeclared_dotted_ssh_host_is_taken_at_face_value`.
fn resolve_ssh_host(host: &str, aliases: &SshHostAliases) -> Result<String, SlugRefusal> {
    if let Some(real) = aliases.hostname_for(host) {
        return Ok(real);
    }
    if host.contains('.') {
        return Ok(host.to_string());
    }
    Err(SlugRefusal::UnresolvedSshAlias(host.to_string()))
}

/// The lowercased host an authority names, without userinfo or port.
///
/// The port addresses the SERVER and the userinfo the CALLER; neither selects a
/// repository, so neither belongs in a `--repo` slug. A trailing `:<digits>` is
/// the only thing treated as a port — `[::1]` keeps its colons because what
/// follows the last one is not all digits.
fn host_from_authority(authority: &str) -> Option<String> {
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = match host.rsplit_once(':') {
        Some((h, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => h,
        _ => host,
    };
    if host.is_empty() || host.chars().any(char::is_whitespace) {
        return None;
    }
    Some(host.to_ascii_lowercase())
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

/// The `[host/]owner/repo` every `gh` call rooted at `dir` must be pinned to
/// (#7057).
///
/// Why: see the module doc. The repository is a property of the DIRECTORY under
/// inspection, so it is read there rather than inherited from the caller, the
/// daemon's working directory, or another registered project.
/// What: `git -C <dir> config --get remote.origin.url` (through
/// [`git_stdout`], which strips the environment able to point git at a
/// different repository), parsed by [`parse_repo_slug`]. A directory with no
/// `origin` of its own falls back to the checkout git names as the owner of its
/// worktree registry — and only there. The result carries the remote's host
/// whenever that is not [`DEFAULT_GH_HOST`]. Every other outcome is an `Err`
/// whose text names the directory and what was read, so a caller can print it.
/// Test: `two_worktrees_with_different_origins_resolve_to_different_repos`,
/// `an_enterprise_worktree_names_its_host_in_the_repo_flag`,
/// `a_directory_with_no_origin_falls_back_to_its_owning_checkout`,
/// `a_non_repository_directory_resolves_to_no_repository`.
pub(crate) fn repo_slug_for(dir: &Path) -> Result<String, String> {
    repo_slug_with(dir, &SshHostAliases::for_current_user())
}

/// [`repo_slug_for`], against a stated SSH alias table (#7196).
///
/// Why: the alias table is the operator's `~/.ssh/config`, which no test may
/// read — a machine whose config renames `github-duetto` and one whose config
/// does not would give the same test two answers. Taking the table as a
/// parameter is how a test states its own, without an environment variable this
/// crate is not allowed to set.
/// What: exactly [`repo_slug_for`]'s body; `repo_slug_for` is this with the
/// current user's table.
/// Test: `an_aliased_origin_resolves_through_the_given_ssh_config`,
/// `an_unresolvable_ssh_alias_refuses_and_names_the_alias`.
pub(crate) fn repo_slug_with(dir: &Path, aliases: &SshHostAliases) -> Result<String, String> {
    match origin_url(dir) {
        Some(url) => parse_repo_slug(&url, aliases).map_err(|r| refusal(dir, &url, &r)),
        None => slug_from_owning_checkout(dir, aliases),
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
fn slug_from_owning_checkout(dir: &Path, aliases: &SshHostAliases) -> Result<String, String> {
    let root = registry_root_for(dir).ok_or_else(|| missing(dir))?;
    if same_directory(&root, dir) {
        return Err(missing(dir));
    }
    let url = origin_url(&root).ok_or_else(|| missing(dir))?;
    parse_repo_slug(&url, aliases).map_err(|r| refusal(&root, &url, &r))
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

/// The refusal text for a remote URL that yields no slug.
///
/// Why: the operator's next action differs by case. An unparseable remote means
/// this is not a GitHub repository; an unresolved alias means one `~/.ssh/config`
/// entry is missing, and naming the alias is what turns a `gh` connection error
/// into an actionable line (#7196).
/// What: one sentence per [`SlugRefusal`] variant, each naming the directory and
/// what was read.
/// Test: `an_unparseable_origin_refuses_and_quotes_the_url`,
/// `an_unresolvable_ssh_alias_refuses_and_names_the_alias`.
fn refusal(dir: &Path, url: &str, why: &SlugRefusal) -> String {
    match why {
        SlugRefusal::NoRepository => format!(
            "the `origin` remote at {} is {url:?}, which names no GitHub `owner/repo` — \
             the lookup is refused rather than aimed at a guess (#7057)",
            dir.display()
        ),
        SlugRefusal::UnresolvedSshAlias(alias) => format!(
            "the `origin` remote at {} is {url:?}, whose SSH host {alias:?} names no machine: \
             `~/.ssh/config` declares no `HostName` for it and it is not a hostname itself. \
             An alias is not a GitHub host, so the lookup is refused rather than aimed at \
             {alias:?} (#7196). Add a `Host {alias}` block with the `HostName` git reaches \
             through it, or point `origin` at that host directly.",
            dir.display()
        ),
    }
}

#[cfg(test)]
#[path = "worktree_repo_slug_tests.rs"]
mod worktree_repo_slug_tests;
