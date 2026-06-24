//! Canonical trusty-memory palace-ID derivation from project identity.
//!
//! Why: the default palace ID should reflect the project's *identity*, not just
//! the bare directory it happens to live in. Two checkouts of the same repo
//! under different directory names should resolve to the same palace, and a
//! GitHub project should be addressable by its canonical `owner/repo` slug.
//! Originally this lived in `trusty-memory` (issue #1217), but trusty-mpm's
//! managed-session MCP injection (issue #1605) must derive the *identical*
//! palace slug to pin `TRUSTY_MEMORY_PALACE` in a cloned session's `.mcp.json`
//! — otherwise a repo-cloned session resolves the wrong palace (its cwd
//! basename is the session-id, not the owner/repo). Hoisting the pure core into
//! `trusty-common` — which both crates already depend on — makes it the single
//! source of truth so the two cannot silently diverge, exactly mirroring how
//! [`crate::derive_index_id`] is shared for trusty-search index pinning (#1373).
//!
//! What: a pure, side-effect-free core — [`derive_palace_id`] — with the
//! precedence **override > git owner/repo > parent/dir**. All filesystem and
//! `git` I/O is kept at the call edges (trusty-memory's `messaging::operations`
//! and trusty-mpm's `session_launch`); this module only parses already-collected
//! inputs (a cwd path, an optional git remote URL, an optional explicit
//! override) so every branch is unit-testable without a runtime, a tempdir, or a
//! real repo.
//!
//! Storage-safety invariant: a palace ID becomes a directory name under the
//! data root (`data_root/<id>`) AND a Unix-socket filename
//! (`trusty-bm25-<id>.sock`). A literal `/` would create nested directories and
//! break socket naming, so the GitHub `owner/repo` form and the `parent/dir`
//! form are slugified with the separator collapsed to `-` (yielding e.g.
//! `bobmatnyc-trusty-tools`, `projects-trusty-tools`). The conceptual
//! `owner/repo` identity is preserved; the on-disk token stays a single
//! kebab-case segment.
//!
//! Test: `cargo test -p trusty-common -- palace_id::tests` exercises every
//! precedence level and every git-URL variant (SSH/HTTPS, with/without `.git`,
//! trailing slashes, non-GitHub hosts, owner-less paths, host:port URLs).

use std::path::Path;

use crate::slug::slugify_string;

/// Environment variable that overrides the derived default palace ID.
///
/// Why: operators need a per-process escape hatch to pin the default palace
/// without editing a pin file or passing `--palace` on every invocation —
/// useful for CI, test rigs, and ephemeral/managed sessions. trusty-mpm's
/// session-launch injection sets this in the cloned session's MCP server `env`
/// block so the spawned trusty-memory resolves the project palace deterministically
/// (issue #1605). An env var complements the existing `--palace` flag (flag wins
/// when both are present, because the flag is resolved first at the call site).
/// What: `"TRUSTY_MEMORY_PALACE"`. When set to a non-empty value it is
/// slugified and used verbatim as the default palace ID.
/// Test: covered by trusty-memory's `messaging` env-override tests, which read
/// this constant; the pure derivation in [`derive_palace_id`] takes the value as
/// a parameter (`override_env_wins_over_git`, `env_override_is_slugified`).
pub const PALACE_OVERRIDE_ENV: &str = "TRUSTY_MEMORY_PALACE";

/// Read the `TRUSTY_MEMORY_PALACE` override from the environment, if set.
///
/// Why: centralises the env lookup so the derivation core stays pure (it
/// receives the already-read value) while call sites get a one-liner. Keeping
/// the read here also documents the single sanctioned env key.
/// What: returns `Some(value)` when `TRUSTY_MEMORY_PALACE` is set to a value
/// that is non-empty after trimming; `None` otherwise (unset, empty, or
/// whitespace-only).
/// Test: side-effect-only (reads process env); the parsing it feeds is covered
/// by `override_env_wins_over_git` and `env_override_is_slugified`.
pub fn palace_override_from_env() -> Option<String> {
    match std::env::var(PALACE_OVERRIDE_ENV) {
        Ok(v) if !v.trim().is_empty() => Some(v),
        _ => None,
    }
}

/// Parse a git remote URL into a slugified, storage-safe `owner-repo` token.
///
/// Why: the canonical identity of a hosted project is its `owner/repo` path,
/// not the local directory name. Deriving the default palace from the remote
/// makes the same repo resolve to the same palace across checkouts, worktrees,
/// and renamed directories. The separator is collapsed to `-` so the result is
/// a single filesystem- and socket-safe segment (see module docs).
/// What: handles the three canonical URL shapes — SSH
/// (`git@github.com:owner/repo.git`), HTTPS
/// (`https://github.com/owner/repo(.git)`), and scp-less host paths /
/// non-GitHub hosts that still expose `owner/repo`. Strips a trailing `.git`,
/// trims trailing slashes, splits the host from the path, takes the final two
/// path segments (`owner`, `repo`), slugifies each, and joins with `-`. When
/// only one trailing segment is parseable (no owner) the repo slug alone is
/// returned. Returns `None` when nothing usable can be extracted (empty input,
/// host-only URL).
/// Test: `git_ssh_github`, `git_https_github_with_and_without_dot_git`,
/// `git_non_github_host`, `git_trailing_slash`, `git_repo_only`,
/// `git_empty_returns_none`, `git_self_hosted_with_port`.
pub fn owner_repo_from_git_remote(url: &str) -> Option<String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Isolate the path portion (everything after the host). The host is
    // delimited by `:` for SSH/scp syntax (`git@host:owner/repo`) and by the
    // first `/` after the scheme's `//` for URL syntax. We normalise by
    // stripping a leading scheme, then splitting on the first `:` or `/` that
    // follows the host.
    let without_scheme = strip_scheme(trimmed);
    let path = host_relative_path(without_scheme);

    // Strip a trailing `.git` and any trailing slashes, then split on `/`.
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let path = path.trim_end_matches('/');

    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if segments.is_empty() {
        return None;
    }

    // Take the last two segments as (owner, repo); if only one is present treat
    // it as the repo with no owner.
    let (owner, repo) = match segments.as_slice() {
        [.., owner, repo] => (Some(*owner), *repo),
        [repo] => (None, *repo),
        _ => return None,
    };

    let repo_slug = slugify_string(repo);
    if repo_slug.is_empty() {
        return None;
    }

    match owner {
        Some(owner) => {
            let owner_slug = slugify_string(owner);
            if owner_slug.is_empty() {
                Some(repo_slug)
            } else {
                Some(format!("{owner_slug}-{repo_slug}"))
            }
        }
        None => Some(repo_slug),
    }
}

/// Strip a leading URL scheme (`https://`, `ssh://`, `git://`, …) if present.
///
/// Why: the path-extraction logic only needs the host-and-path portion; the
/// scheme is noise that complicates the `:` vs `/` host split (a scheme's `://`
/// colon must not be mistaken for the SSH host delimiter).
/// What: returns the input with everything up to and including a leading
/// `<scheme>://` removed. Inputs without a scheme (SSH scp-syntax like
/// `git@host:owner/repo`) are returned unchanged.
/// Test: covered indirectly by `git_https_github_with_and_without_dot_git`
/// (scheme present) and `git_ssh_github` (scheme absent).
fn strip_scheme(url: &str) -> &str {
    match url.find("://") {
        Some(idx) => &url[idx + 3..],
        None => url,
    }
}

/// Reduce a host-prefixed locator to just its path portion.
///
/// Why: both `host/owner/repo` (URL) and `host:owner/repo` (SSH scp-syntax)
/// carry the host as a leading component that must be dropped before we take
/// the trailing `owner/repo` segments. Self-hosted URLs with explicit ports
/// (`host:8080/owner/repo`) must not be mistaken for SSH scp-syntax — the
/// port-number segment after the colon is not an owner path.
/// What: if the locator contains an SSH `:` separator before any `/` AND the
/// text immediately after the colon is NOT a pure-digit port number, splits on
/// that `:` and returns the remainder (SSH scp-syntax); if the colon is
/// followed by only digits before the next `/`, treats the whole `host:port`
/// prefix as the host component and drops up through that first `/` (URL
/// style with explicit port). Otherwise drops the first `/`-delimited segment
/// (the host). A locator with no separators is returned unchanged.
/// Test: `git_ssh_github`, `git_https_github_*`, `git_non_github_host`,
/// `git_self_hosted_with_port`.
fn host_relative_path(locator: &str) -> &str {
    let colon = locator.find(':');
    let slash = locator.find('/');
    match (colon, slash) {
        // Colon precedes the first slash — could be SSH or host:port.
        (Some(c), maybe_slash) if maybe_slash.is_none_or(|s| c < s) => {
            let after_colon = &locator[c + 1..];
            // Check whether the text after the colon up to the next '/' is
            // all digits — that means this is a `host:port/path` URL, not
            // SSH scp-syntax `host:owner/repo`.
            let port_end = after_colon.find('/').unwrap_or(after_colon.len());
            let potential_port = &after_colon[..port_end];
            if !potential_port.is_empty() && potential_port.bytes().all(|b| b.is_ascii_digit()) {
                // URL with explicit port: drop host:port/ prefix.
                match after_colon.find('/') {
                    Some(s) => &after_colon[s + 1..],
                    None => "",
                }
            } else {
                // SSH scp-syntax: return everything after the colon.
                after_colon
            }
        }
        // URL-style `host/owner/repo` — drop the leading host segment.
        (_, Some(s)) => &locator[s + 1..],
        // No path separators at all — nothing to strip.
        _ => locator,
    }
}

/// Derive a `parent-dir` slug from the last two components of a project root.
///
/// Why: outside a git context the project's identity is best approximated by
/// its directory plus the directory above it (`Projects/trusty-tools`), which
/// disambiguates same-named leaf directories under different parents. As with
/// the git form, the separator is collapsed to `-` for storage safety.
/// What: takes the final path component and its parent's final component,
/// slugifies each, and joins with `-`. When the path has only one component
/// (filesystem root child) the leaf slug alone is returned. Returns `None`
/// when no usable leaf component exists.
/// Test: `parent_dir_two_components`, `parent_dir_single_component`,
/// `parent_dir_root_returns_none`.
pub fn parent_dir_slug(root: &Path) -> Option<String> {
    let leaf = root.file_name().and_then(|s| s.to_str())?;
    let leaf_slug = slugify_string(leaf);
    if leaf_slug.is_empty() {
        return None;
    }

    let parent_slug = root
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .map(slugify_string)
        .filter(|s| !s.is_empty());

    match parent_slug {
        Some(parent) => Some(format!("{parent}-{leaf_slug}")),
        None => Some(leaf_slug),
    }
}

/// Derive the default palace ID from project identity (issue #1217 core).
///
/// Why: the single pure decision point for the default palace ID, so the
/// precedence rule lives in one tested place rather than being scattered across
/// the hook, CLI, MCP, and (issue #1605) trusty-mpm session-launch call sites.
/// Keeping it pure (no FS, no `git`, no env reads) makes every branch
/// deterministically testable.
/// What: applies **three** of the four full-stack precedence levels documented
/// in `docs/reference/environment-variables.md` for `TRUSTY_MEMORY_PALACE`:
/// (1) `override_value` — the already-resolved `--palace` flag or
/// `TRUSTY_MEMORY_PALACE` env read by the call site — slugified and winning
/// unconditionally; (2) `git_remote` parsed via [`owner_repo_from_git_remote`]
/// (git owner/repo slug); (3) [`parent_dir_slug`] of `project_root`
/// (parent/dir slug). The fourth level — a committed
/// `.trusty-tools/trusty-memory.yaml` pin file — is handled above this
/// function in trusty-memory's `cwd_palace_slug_at` because it requires
/// filesystem I/O that this pure core intentionally avoids. Returns `None` only
/// when none of the three yields a non-empty slug (e.g. an empty override, an
/// unparseable remote, and a root-less path all at once).
/// Test: `override_env_wins_over_git`, `git_used_when_no_override`,
/// `falls_back_to_parent_dir`, `all_empty_returns_none`.
pub fn derive_palace_id(
    project_root: &Path,
    git_remote: Option<&str>,
    override_value: Option<&str>,
) -> Option<String> {
    // 1. Explicit override always wins.
    if let Some(ov) = override_value {
        let slug = slugify_string(ov);
        if !slug.is_empty() {
            return Some(slug);
        }
    }

    // 2. Git owner/repo identity.
    if let Some(url) = git_remote {
        if let Some(slug) = owner_repo_from_git_remote(url) {
            return Some(slug);
        }
    }

    // 3. parent/dir fallback.
    parent_dir_slug(project_root)
}

#[cfg(test)]
mod tests {
    //! Unit tests for the palace-ID derivation core.
    //!
    //! These pure helpers (`derive_palace_id`, `owner_repo_from_git_remote`,
    //! `parent_dir_slug`) do **not** read environment variables — they receive
    //! the already-resolved override value as a parameter. That reading happens
    //! in [`super::palace_override_from_env`], whose env-mutation behaviour is
    //! exercised at a higher level by trusty-memory's `messaging` tests (which
    //! serialise env access with `#[serial_test::serial]`). Adding any test here
    //! that mutates `TRUSTY_MEMORY_PALACE` would require the same serialisation.

    use super::*;
    use std::path::{Path, PathBuf};

    // -----------------------------------------------------------------------
    // owner_repo_from_git_remote — URL variants
    // -----------------------------------------------------------------------

    /// Why: SSH scp-syntax is the most common GitHub remote form; it must
    /// parse to the slugified `owner-repo` token.
    /// Test: itself.
    #[test]
    fn git_ssh_github() {
        assert_eq!(
            owner_repo_from_git_remote("git@github.com:bobmatnyc/trusty-tools.git").as_deref(),
            Some("bobmatnyc-trusty-tools")
        );
    }

    /// Why: HTTPS remotes appear with and without the trailing `.git`; both
    /// must collapse to the same token.
    /// Test: itself.
    #[test]
    fn git_https_github_with_and_without_dot_git() {
        assert_eq!(
            owner_repo_from_git_remote("https://github.com/bobmatnyc/trusty-tools.git").as_deref(),
            Some("bobmatnyc-trusty-tools")
        );
        assert_eq!(
            owner_repo_from_git_remote("https://github.com/bobmatnyc/trusty-tools").as_deref(),
            Some("bobmatnyc-trusty-tools")
        );
    }

    /// Why: non-GitHub hosts (GitLab, self-hosted) still expose `owner/repo`;
    /// the host is irrelevant to the slug.
    /// Test: itself.
    #[test]
    fn git_non_github_host() {
        assert_eq!(
            owner_repo_from_git_remote("git@gitlab.example.com:acme/Cool_App.git").as_deref(),
            Some("acme-cool-app")
        );
        assert_eq!(
            owner_repo_from_git_remote("https://gitlab.example.com/acme/Cool_App").as_deref(),
            Some("acme-cool-app")
        );
    }

    /// Why: self-hosted git servers often bind to a non-default port (e.g.
    /// `https://gitlab.company.com:8080/owner/repo.git`). The port-detection
    /// branch in `host_relative_path` must not mistake `8080` for an SSH
    /// scp-style `host:path` separator. Regression-guard for issue #1228.
    /// Test: itself.
    #[test]
    fn git_self_hosted_with_port() {
        // Single-level repo path — the common case that caused the mis-parse.
        assert_eq!(
            owner_repo_from_git_remote("https://git.company.com:8080/repo.git").as_deref(),
            Some("repo")
        );
        // With an owner segment — should resolve owner-repo, not 8080-repo.
        assert_eq!(
            owner_repo_from_git_remote("https://git.company.com:8080/owner/repo.git").as_deref(),
            Some("owner-repo")
        );
        // Trailing slash variant.
        assert_eq!(
            owner_repo_from_git_remote("https://git.company.com:8080/owner/repo/").as_deref(),
            Some("owner-repo")
        );
        // SSH scp-syntax must still work — the colon is followed by a non-digit.
        assert_eq!(
            owner_repo_from_git_remote("git@git.company.com:owner/repo.git").as_deref(),
            Some("owner-repo")
        );
    }

    /// Why: a trailing slash on an HTTPS URL must not produce an empty repo
    /// segment or a dangling separator.
    /// Test: itself.
    #[test]
    fn git_trailing_slash() {
        assert_eq!(
            owner_repo_from_git_remote("https://github.com/bobmatnyc/trusty-tools/").as_deref(),
            Some("bobmatnyc-trusty-tools")
        );
    }

    /// Why: nested group paths (GitLab subgroups) should still resolve to the
    /// final two segments — the immediate owner/group and the repo.
    /// Test: itself.
    #[test]
    fn git_nested_group_takes_last_two() {
        assert_eq!(
            owner_repo_from_git_remote("https://gitlab.com/acme/team/widget.git").as_deref(),
            Some("team-widget")
        );
    }

    /// Why: a URL exposing only a repo segment (no owner) must still yield the
    /// repo slug rather than `None`.
    /// Test: itself.
    #[test]
    fn git_repo_only() {
        assert_eq!(
            owner_repo_from_git_remote("git@host:repo.git").as_deref(),
            Some("repo")
        );
    }

    /// Why: empty / host-only inputs have no extractable identity and must
    /// return `None` so the caller falls through to the parent/dir form.
    /// Test: itself.
    #[test]
    fn git_empty_returns_none() {
        assert_eq!(owner_repo_from_git_remote(""), None);
        assert_eq!(owner_repo_from_git_remote("   "), None);
        // Host-only (no path segments).
        assert_eq!(owner_repo_from_git_remote("https://github.com/"), None);
    }

    // -----------------------------------------------------------------------
    // parent_dir_slug
    // -----------------------------------------------------------------------

    /// Why: the canonical fallback — last two path components joined and
    /// slugified (`Projects/trusty-tools` → `projects-trusty-tools`).
    /// Test: itself.
    #[test]
    fn parent_dir_two_components() {
        assert_eq!(
            parent_dir_slug(Path::new("/Users/bob/Projects/trusty-tools")).as_deref(),
            Some("projects-trusty-tools")
        );
    }

    /// Why: casing and underscores must be normalised in both components so
    /// `My_Org/Cool_App` matches its kebab form.
    /// Test: itself.
    #[test]
    fn parent_dir_normalises_case_and_underscores() {
        assert_eq!(
            parent_dir_slug(Path::new("/x/My_Org/Cool_App")).as_deref(),
            Some("my-org-cool-app")
        );
    }

    /// Why: a single-component path (a child of the filesystem root) has no
    /// parent component; the leaf slug alone must be returned.
    /// Test: itself.
    #[test]
    fn parent_dir_single_component() {
        assert_eq!(parent_dir_slug(Path::new("/solo")).as_deref(), Some("solo"));
    }

    /// Why: the filesystem root has no final component; derivation must yield
    /// `None` so the caller can surface an actionable error.
    /// Test: itself.
    #[test]
    fn parent_dir_root_returns_none() {
        assert_eq!(parent_dir_slug(Path::new("/")), None);
    }

    // -----------------------------------------------------------------------
    // derive_palace_id — precedence
    // -----------------------------------------------------------------------

    /// Why: an explicit override must beat a parseable git remote (precedence
    /// level 1 > level 2).
    /// Test: itself.
    #[test]
    fn override_env_wins_over_git() {
        let root = PathBuf::from("/Users/bob/Projects/trusty-tools");
        let got = derive_palace_id(
            &root,
            Some("git@github.com:bobmatnyc/trusty-tools.git"),
            Some("my-override"),
        );
        assert_eq!(got.as_deref(), Some("my-override"));
    }

    /// Why: the override value must be slugified, not passed through verbatim,
    /// so an operator typo like `My Project` cannot create an unsafe ID.
    /// Test: itself.
    #[test]
    fn env_override_is_slugified() {
        let root = PathBuf::from("/x/y");
        let got = derive_palace_id(&root, None, Some("My Project_Name"));
        assert_eq!(got.as_deref(), Some("my-project-name"));
    }

    /// Why: an empty/whitespace override must be ignored so derivation falls
    /// through to git (not return an empty ID).
    /// Test: itself.
    #[test]
    fn empty_override_falls_through_to_git() {
        let root = PathBuf::from("/x/y");
        let got = derive_palace_id(&root, Some("git@github.com:acme/widget.git"), Some("   "));
        assert_eq!(got.as_deref(), Some("acme-widget"));
    }

    /// Why: with no override, a parseable git remote wins over the directory
    /// (precedence level 2 > level 3) — the core identity-from-git behaviour.
    /// Test: itself.
    #[test]
    fn git_used_when_no_override() {
        let root = PathBuf::from("/some/checkout-dir");
        let got = derive_palace_id(
            &root,
            Some("https://github.com/bobmatnyc/trusty-tools.git"),
            None,
        );
        assert_eq!(got.as_deref(), Some("bobmatnyc-trusty-tools"));
    }

    /// Why: with no override and no usable remote, derivation must fall back to
    /// the parent/dir slug (precedence level 3).
    /// Test: itself.
    #[test]
    fn falls_back_to_parent_dir() {
        let root = PathBuf::from("/Users/bob/Projects/trusty-tools");
        assert_eq!(
            derive_palace_id(&root, None, None).as_deref(),
            Some("projects-trusty-tools")
        );
        // An unparseable remote also falls through to parent/dir.
        assert_eq!(
            derive_palace_id(&root, Some(""), None).as_deref(),
            Some("projects-trusty-tools")
        );
    }

    /// Why: when every source is exhausted (empty override, empty remote,
    /// root-less path) the function must return `None` so the caller errors
    /// rather than inventing an empty palace ID.
    /// Test: itself.
    #[test]
    fn all_empty_returns_none() {
        assert_eq!(
            derive_palace_id(Path::new("/"), Some(""), Some("   ")),
            None
        );
    }
}
