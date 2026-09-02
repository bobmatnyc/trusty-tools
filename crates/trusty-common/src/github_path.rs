//! Shared GitHub `owner/repo` path derivation (issue #1220).
//!
//! Why: two trusty-* subsystems need the canonical `owner/repo` identity of a
//! project, derived from its git origin remote: trusty-mpm's managed-session
//! workspace root (`~/trusty-mpm-projects/<owner>/<repo>/…`, #1220) and
//! trusty-memory's palace-ID derivation (#1217). Before this module each crate
//! re-implemented git-URL parsing; centralising it in `trusty-common` gives both
//! one tested seam and guarantees they agree on what `<owner>/<repo>` means for
//! a given remote. Unlike trusty-memory's `owner_repo_from_git_remote` (which
//! collapses to a single storage-safe `owner-repo` token), this module keeps
//! `owner` and `repo` as SEPARATE components because #1220 maps them onto two
//! nested filesystem path segments.
//!
//! What: [`GithubPath`] is the parsed `{ owner, repo }` pair; [`parse_github_path`]
//! turns a git remote URL into one (pure, no I/O); [`derive_github_path`] runs
//! `git config --get remote.origin.url` in a directory and parses the result
//! (the only I/O entry point). Both components are slugified for filesystem
//! safety — lower-cased, non-alphanumerics collapsed to `-`, trailing `.git`
//! stripped — so the result is always two clean path segments.
//!
//! [`RemoteRepo`] and [`parse_remote_url`] are the same parse WITHOUT the
//! slugification, added by #6657: they keep the owner and repo exactly as the
//! remote spells them and carry the host, because their callers hand the pair
//! to the GitHub API or use it as an object-storage key rather than as a path.
//! That pair is the workspace's single git-remote-URL parser —
//! `trusty-git-analytics` and `trusty-code` route through it instead of each
//! carrying their own.
//!
//! Test: `parse_*` unit tests cover SSH/HTTPS, with/without `.git`, trailing
//! slashes, nested groups, owner-less and empty inputs; `parse_remote_url_table`
//! covers the verbatim parser; `derive_*` is covered by
//! `derive_github_path_reads_origin` and `derive_remote_repo_reads_origin`
//! against real temp git repos.
//!
//! [`GithubPath`]: crate::github_path::GithubPath
//! [`parse_github_path`]: crate::github_path::parse_github_path
//! [`derive_github_path`]: crate::github_path::derive_github_path
//! [`RemoteRepo`]: crate::github_path::RemoteRepo
//! [`parse_remote_url`]: crate::github_path::parse_remote_url

use std::path::Path;
use std::process::Command;

/// A parsed, slugified GitHub-style project identity.
///
/// Why: #1220's workspace-root convention nests sessions under
/// `~/trusty-mpm-projects/<owner>/<repo>/`, so callers need the owner and the
/// repo as two independent, filesystem-safe segments — not a single fused token.
/// Keeping them in a struct (rather than a tuple) makes call sites self-documenting
/// and lets the type grow (e.g. a `host` field) without breaking signatures.
/// What: the slugified `owner` and `repo` path segments. Both are guaranteed
/// non-empty by every constructor in this module (a parse that cannot produce a
/// non-empty `repo` returns `None`); `owner` is `"unknown-owner"` only via the
/// explicit owner-less fallback documented on [`parse_github_path`].
/// Test: every `parse_*` test asserts the two fields independently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GithubPath {
    /// Slugified repository owner (GitHub user or org), e.g. `bobmatnyc`.
    pub owner: String,
    /// Slugified repository name, e.g. `trusty-tools`.
    pub repo: String,
}

impl GithubPath {
    /// Render the identity as the relative path `<owner>/<repo>`.
    ///
    /// Why: the workspace-root builder joins this onto the configured root; a
    /// single accessor keeps the join order (`owner` then `repo`) in one place so
    /// callers cannot accidentally swap the segments.
    /// What: returns `format!("{owner}/{repo}")` — always using `/` so the result
    /// is a portable relative path that `Path::join` splits into two components.
    /// Test: `github_path_rel_joins_owner_repo`.
    pub fn rel_path(&self) -> String {
        format!("{}/{}", self.owner, self.repo)
    }
}

/// Slugify a single path component for filesystem safety.
///
/// Why: owners and repo names can contain mixed case, underscores, and (rarely)
/// other punctuation; turning each into a clean kebab-case segment keeps the
/// derived workspace path predictable and avoids surprising directory names.
/// What: lower-cases, strips a trailing `.git`, maps `[a-z0-9]` through, collapses
/// any run of other characters to a single `-`, and trims leading/trailing `-`.
/// Returns an empty string when nothing survives. Mirrors trusty-memory's
/// `slugify_string` so the two crates derive identical tokens for the same input.
/// Test: exercised indirectly by every `parse_*` test (case, underscores,
/// `.git`, punctuation).
fn slugify_component(input: &str) -> String {
    let lowered = input.trim().to_ascii_lowercase();
    let stripped = lowered.strip_suffix(".git").unwrap_or(&lowered);
    let mut out = String::with_capacity(stripped.len());
    let mut prev_hyphen = false;
    for c in stripped.chars() {
        match c {
            'a'..='z' | '0'..='9' => {
                out.push(c);
                prev_hyphen = false;
            }
            _ => {
                // Collapse any run of separators/punctuation into one hyphen,
                // never leading.
                if !prev_hyphen && !out.is_empty() {
                    out.push('-');
                    prev_hyphen = true;
                }
            }
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// Strip a leading URL scheme (`https://`, `ssh://`, `git://`, …) if present.
///
/// Why: the path-extraction logic only needs the host-and-path portion; a
/// scheme's `://` colon must not be mistaken for the SSH host delimiter.
/// What: returns everything after a leading `<scheme>://`; inputs without a
/// scheme (SSH scp-syntax like `git@host:owner/repo`) are returned unchanged.
/// Test: covered by `parse_https_*` (scheme present) and `parse_ssh_*` (absent).
fn strip_scheme(url: &str) -> &str {
    match url.find("://") {
        Some(idx) => &url[idx + 3..],
        None => url,
    }
}

/// Reduce a host-prefixed locator to just its path portion.
///
/// Why: both `host/owner/repo` (URL) and `host:owner/repo` (SSH scp-syntax)
/// carry the host as a leading component that must be dropped before taking the
/// trailing `owner/repo` segments.
/// What: if an SSH `:` separator precedes the first `/`, splits on it and returns
/// the remainder; otherwise drops the first `/`-delimited segment (the host). A
/// locator with no separators is returned unchanged.
/// Test: covered by `parse_ssh_github`, `parse_https_github_*`.
fn host_relative_path(locator: &str) -> &str {
    let colon = locator.find(':');
    let slash = locator.find('/');
    match (colon, slash) {
        (Some(c), maybe_slash) if maybe_slash.is_none_or(|s| c < s) => &locator[c + 1..],
        (_, Some(s)) => &locator[s + 1..],
        _ => locator,
    }
}

/// Fallback owner used when a remote exposes a repo segment but no owner.
///
/// Why: #1220 nests sessions two levels deep (`<owner>/<repo>`); an owner-less
/// remote (e.g. `git@host:repo.git`) would otherwise yield a one-level path and
/// break the convention. A stable sentinel keeps the two-segment shape.
/// What: `"unknown-owner"` — already slug-safe.
/// Test: `parse_repo_only_uses_unknown_owner`.
pub const UNKNOWN_OWNER: &str = "unknown-owner";

/// Parse a git remote URL into a [`GithubPath`] (`{ owner, repo }`).
///
/// Why: the canonical identity of a hosted project is the `owner/repo` path in
/// its remote URL, not the local directory name. Parsing it purely (no I/O) makes
/// every URL-shape branch deterministically unit-testable.
/// What: handles the three canonical shapes — SSH (`git@github.com:owner/repo.git`),
/// HTTPS (`https://github.com/owner/repo(.git)`), and scp-less host paths — by
/// stripping the scheme + host, trimming a trailing `.git`/slashes, splitting on
/// `/`, and taking the final two segments as `(owner, repo)`. Each segment is
/// slugified. When only one trailing segment is parseable (no owner) the repo is
/// kept and `owner` falls back to [`UNKNOWN_OWNER`] so the result is always a
/// two-segment path. Returns `None` only when no non-empty `repo` slug can be
/// produced (empty input, host-only URL).
/// Test: `parse_ssh_github`, `parse_https_github_with_and_without_dot_git`,
/// `parse_non_github_host`, `parse_trailing_slash`, `parse_nested_group_takes_last_two`,
/// `parse_repo_only_uses_unknown_owner`, `parse_empty_returns_none`.
pub fn parse_github_path(url: &str) -> Option<GithubPath> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return None;
    }

    let path = host_relative_path(strip_scheme(trimmed));
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let path = path.trim_end_matches('/');

    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let (owner_raw, repo_raw) = match segments.as_slice() {
        [.., owner, repo] => (Some(*owner), *repo),
        [repo] => (None, *repo),
        _ => return None,
    };

    let repo = slugify_component(repo_raw);
    if repo.is_empty() {
        return None;
    }

    let owner = owner_raw
        .map(slugify_component)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| UNKNOWN_OWNER.to_string());

    Some(GithubPath { owner, repo })
}

/// Parse an already-canonical `owner/repo` string into a [`GithubPath`].
///
/// Why: [`parse_github_path`] expects a git *URL* — it strips a leading host
/// segment, so feeding it a bare `owner/repo` would wrongly drop `owner` as if
/// it were a host. Round-tripping a stored canonical identity (e.g. the
/// `?repo_identity=owner/repo` daemon filter, or a value read back from
/// `indexes.toml`) needs a parser that treats the input as a path, not a URL.
/// What: rejects input containing `@` or `://` up front — those are unambiguous
/// markers of a URL/SSH remote (`git@host:owner/repo`, `https://host/owner/repo`),
/// which this function must NOT silently mis-slugify into a nonsense identity
/// (e.g. `git@host:owner/repo` would otherwise parse as owner=`host:owner`,
/// repo=`repo`). Callers that genuinely have a remote URL must use
/// [`parse_github_path`] instead. Otherwise splits on `/`, keeps the last two
/// non-empty segments as `(owner, repo)` (so nested group paths collapse to
/// their final two, matching [`parse_github_path`]), and slugifies each. A
/// single trailing segment maps to `(UNKNOWN_OWNER, repo)`. Returns `None` when
/// no non-empty `repo` slug survives, or the URL/SSH-shape guard rejects it.
/// Test: `parse_owner_repo_roundtrips_rel_path`, `parse_owner_repo_single_segment`,
/// `parse_owner_repo_empty_returns_none`, `parse_owner_repo_rejects_url_shaped_input`.
pub fn parse_owner_repo(s: &str) -> Option<GithubPath> {
    let trimmed = s.trim();
    if trimmed.contains('@') || trimmed.contains("://") {
        return None;
    }
    let segments: Vec<&str> = trimmed.split('/').filter(|p| !p.is_empty()).collect();
    let (owner_raw, repo_raw) = match segments.as_slice() {
        [.., owner, repo] => (Some(*owner), *repo),
        [repo] => (None, *repo),
        _ => return None,
    };
    let repo = slugify_component(repo_raw);
    if repo.is_empty() {
        return None;
    }
    let owner = owner_raw
        .map(slugify_component)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| UNKNOWN_OWNER.to_string());
    Some(GithubPath { owner, repo })
}

/// Derive a [`GithubPath`] from the git origin remote of a directory.
///
/// Why: the only I/O entry point — the workspace-root builder needs the
/// `owner/repo` identity of the repo a session targets, which lives in its
/// `remote.origin.url`. Shelling out to `git config` (rather than reading
/// `.git/config`) transparently handles worktrees, where `.git` is a file
/// pointing at the parent repo.
/// What: runs `git -C <dir> config --get remote.origin.url`; on success parses
/// the URL via [`parse_github_path`]. Returns `None` when git is absent, `dir` is
/// not a repo, there is no origin remote, or the URL has no parseable identity.
/// Best-effort, no network.
/// Test: `derive_github_path_reads_origin` (temp repo with an origin remote);
/// `derive_github_path_none_outside_repo`.
pub fn derive_github_path(dir: &Path) -> Option<GithubPath> {
    origin_remote_url(dir).and_then(|url| parse_github_path(url.trim()))
}

/// Read `remote.origin.url` for the repo containing `dir`.
///
/// Shelling out to `git config` (rather than reading `.git/config`) is what
/// makes this work inside a worktree, where `.git` is a file pointing at the
/// parent repo, and inside a SUBDIRECTORY of a repo, where git walks up to the
/// enclosing checkout. Returns `None` when git is absent, `dir` is not in a
/// repo, or there is no origin remote.
/// Test: `derive_github_path_reads_origin`, `derive_remote_repo_reads_origin`.
fn origin_remote_url(dir: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .arg("config")
        .arg("--get")
        .arg("remote.origin.url")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// A git remote URL split into its host and its `owner/repo` pair, VERBATIM.
///
/// Why: [`GithubPath`] slugifies, because its consumers build filesystem paths
/// from it. Three other call sites need the opposite — the identity exactly as
/// the remote spells it, because they hand it to the GitHub API or use it as an
/// object-storage key (#6657). Before this type each of them re-implemented git
/// URL parsing: `tga`'s `extract_owner_repo_from_url` (twice) and
/// `trusty-code`'s `mpm_registry::parse_owner_repo`.
/// What: `host` carries any port and never the `user@` prefix; `owner` and
/// `repo` keep their original case and punctuation, minus a trailing `.git`.
/// A caller that only accepts one forge filters on `host` itself.
/// Test: `parse_remote_url_table`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RemoteRepo {
    /// Remote host, with a port when the URL carried one, e.g. `github.com`.
    pub host: String,
    /// Repository owner exactly as the remote spells it, e.g. `duettoresearch`.
    pub owner: String,
    /// Repository name exactly as the remote spells it, minus a trailing `.git`.
    pub repo: String,
}

impl RemoteRepo {
    /// Render the identity as `<owner>/<repo>`.
    pub fn owner_repo(&self) -> String {
        format!("{}/{}", self.owner, self.repo)
    }
}

/// Why a git remote URL yielded no `owner/repo`.
///
/// Why: the log drain refuses to upload under a guessed key, so it has to tell
/// an operator which source could not be identified and what was wrong with it
/// (#6657). An `Option` would name neither.
/// What: hand-written `Display`/`Error` rather than `thiserror`, because
/// [`mod@crate::github_path`] is compiled unconditionally and `thiserror` is an
/// optional dependency of this crate.
/// Test: `parse_remote_url_table`, `derive_remote_repo_without_origin`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RemoteUrlError {
    /// The string is not a git remote URL naming exactly one `owner/repo`.
    Malformed {
        /// The input, so the message names what the operator wrote.
        url: String,
        /// Which part of the grammar failed.
        reason: &'static str,
    },
    /// The directory is not a git checkout, or has no `origin` remote.
    NoOrigin {
        /// The directory that was probed.
        dir: String,
    },
}

impl std::fmt::Display for RemoteUrlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed { url, reason } => {
                write!(f, "`{url}` is not a git remote URL: {reason}")
            }
            Self::NoOrigin { dir } => {
                write!(f, "{dir} is not a git checkout with an `origin` remote")
            }
        }
    }
}

impl std::error::Error for RemoteUrlError {}

/// Parse a git remote URL into its host and `owner/repo`, preserving case.
///
/// Why: the single implementation of git-remote-URL parsing for this workspace
/// (#6657). See [`RemoteRepo`] for why [`parse_github_path`] does not serve
/// these callers.
/// What: accepts `https://host/owner/repo`, `http://…`, `ssh://[user@]host[:port]/owner/repo`,
/// and the scp-style `[user@]host:owner/repo`, each with or without a trailing
/// `.git` and trailing slashes. A `user@` prefix is dropped; a port stays with
/// the host. The path must be EXACTLY two non-empty segments, so a GitLab
/// subgroup path (`group/subgroup/repo`) is refused rather than collapsed into
/// an owner containing a slash. Everything else — a bare word, a filesystem
/// path, a host with no path — is [`RemoteUrlError::Malformed`].
/// Test: `parse_remote_url_table`.
///
/// # Errors
/// [`RemoteUrlError::Malformed`], naming the input and the reason.
pub fn parse_remote_url(url: &str) -> Result<RemoteRepo, RemoteUrlError> {
    let trimmed = url.trim();
    let malformed = |reason: &'static str| RemoteUrlError::Malformed {
        url: trimmed.to_string(),
        reason,
    };
    if trimmed.is_empty() {
        return Err(malformed("it is empty"));
    }

    // With a scheme the host ends at the first `/`; without one this is
    // scp-syntax and the host ends at the first `:`. Splitting on the wrong
    // one is how `host:PORT/owner/repo` used to parse as owner `PORT`.
    let (authority, path) = match trimmed.split_once("://") {
        Some((_, rest)) => rest
            .split_once('/')
            .ok_or_else(|| malformed("the URL has no path after the host"))?,
        None => trimmed
            .split_once(':')
            .ok_or_else(|| malformed("expected `scheme://host/owner/repo` or `host:owner/repo`"))?,
    };

    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    if host.is_empty() {
        return Err(malformed("the host is empty"));
    }

    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let path = path.trim_end_matches('/');
    let segments: Vec<&str> = path.split('/').collect();
    let [owner, repo] = segments.as_slice() else {
        return Err(malformed("the path is not exactly `<owner>/<repo>`"));
    };
    if owner.is_empty() || repo.is_empty() {
        return Err(malformed("the owner or the repository name is empty"));
    }

    Ok(RemoteRepo {
        host: host.to_string(),
        owner: (*owner).to_string(),
        repo: (*repo).to_string(),
    })
}

/// Read a directory's git `origin` remote and parse it with [`parse_remote_url`].
///
/// Why: the drain resolves a log source's `owner/project` from the checkout the
/// logs belong to, and must fail rather than invent a key when it cannot
/// (#6657).
/// What: runs `git -C <dir> config --get remote.origin.url`, so a subdirectory
/// of a repo resolves to the enclosing checkout and a worktree resolves to its
/// parent. No network.
/// Test: `derive_remote_repo_reads_origin`, `derive_remote_repo_without_origin`.
///
/// # Errors
/// [`RemoteUrlError::NoOrigin`] when `dir` is not a checkout or has no origin;
/// [`RemoteUrlError::Malformed`] when the origin URL does not parse.
pub fn derive_remote_repo(dir: &Path) -> Result<RemoteRepo, RemoteUrlError> {
    let url = origin_remote_url(dir).ok_or_else(|| RemoteUrlError::NoOrigin {
        dir: dir.display().to_string(),
    })?;
    parse_remote_url(&url)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: SSH scp-syntax is the most common GitHub remote; owner and repo must
    /// be split into two slugified segments.
    /// Test: itself.
    #[test]
    fn parse_ssh_github() {
        let gp = parse_github_path("git@github.com:bobmatnyc/trusty-tools.git").expect("parsed");
        assert_eq!(gp.owner, "bobmatnyc");
        assert_eq!(gp.repo, "trusty-tools");
    }

    /// Why: HTTPS remotes appear with and without the trailing `.git`; both must
    /// yield the same two segments.
    /// Test: itself.
    #[test]
    fn parse_https_github_with_and_without_dot_git() {
        let with = parse_github_path("https://github.com/bobmatnyc/trusty-tools.git").unwrap();
        let without = parse_github_path("https://github.com/bobmatnyc/trusty-tools").unwrap();
        assert_eq!(with, without);
        assert_eq!(with.owner, "bobmatnyc");
        assert_eq!(with.repo, "trusty-tools");
    }

    /// Why: non-GitHub hosts still expose `owner/repo`; the host is irrelevant
    /// and mixed-case/underscores must be normalised in both segments.
    /// Test: itself.
    #[test]
    fn parse_non_github_host() {
        let gp = parse_github_path("git@gitlab.example.com:Acme/Cool_App.git").unwrap();
        assert_eq!(gp.owner, "acme");
        assert_eq!(gp.repo, "cool-app");
    }

    /// Why: a trailing slash must not produce an empty repo segment.
    /// Test: itself.
    #[test]
    fn parse_trailing_slash() {
        let gp = parse_github_path("https://github.com/bobmatnyc/trusty-tools/").unwrap();
        assert_eq!(gp.owner, "bobmatnyc");
        assert_eq!(gp.repo, "trusty-tools");
    }

    /// Why: nested group paths (GitLab subgroups) must resolve to the final two
    /// segments — the immediate group and the repo.
    /// Test: itself.
    #[test]
    fn parse_nested_group_takes_last_two() {
        let gp = parse_github_path("https://gitlab.com/acme/team/widget.git").unwrap();
        assert_eq!(gp.owner, "team");
        assert_eq!(gp.repo, "widget");
    }

    /// Why: an owner-less remote must still yield a two-segment path so the
    /// `<owner>/<repo>` convention holds; owner falls back to the sentinel.
    /// Test: itself.
    #[test]
    fn parse_repo_only_uses_unknown_owner() {
        let gp = parse_github_path("git@host:repo.git").unwrap();
        assert_eq!(gp.owner, UNKNOWN_OWNER);
        assert_eq!(gp.repo, "repo");
    }

    /// Why: empty / host-only inputs have no extractable identity and must
    /// return `None` so the caller falls back to a slug/default.
    /// Test: itself.
    #[test]
    fn parse_empty_returns_none() {
        assert_eq!(parse_github_path(""), None);
        assert_eq!(parse_github_path("   "), None);
        assert_eq!(parse_github_path("https://github.com/"), None);
    }

    /// Why: callers join the identity onto a root; `rel_path` must emit
    /// `<owner>/<repo>` in that exact order.
    /// Test: itself.
    #[test]
    fn github_path_rel_joins_owner_repo() {
        let gp = GithubPath {
            owner: "bobmatnyc".into(),
            repo: "trusty-tools".into(),
        };
        assert_eq!(gp.rel_path(), "bobmatnyc/trusty-tools");
    }

    /// Why: the I/O entry point must read a real `remote.origin.url` and parse it;
    /// uses a throwaway temp repo so it never touches the network.
    /// Test: itself (skips cleanly if `git` is unavailable on the runner).
    #[test]
    fn derive_github_path_reads_origin() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let dir = tmp.path();
        let git = |args: &[&str]| {
            Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        };
        if !git(&["init"]) {
            // No usable git on this runner — nothing to assert.
            return;
        }
        let _ = git(&[
            "remote",
            "add",
            "origin",
            "git@github.com:bobmatnyc/trusty-tools.git",
        ]);
        let gp = derive_github_path(dir).expect("derived from origin");
        assert_eq!(gp.owner, "bobmatnyc");
        assert_eq!(gp.repo, "trusty-tools");
    }

    /// Why: a directory that is not a git repo must yield `None`, not panic, so
    /// the caller can fall back cleanly.
    /// Test: itself.
    #[test]
    fn derive_github_path_none_outside_repo() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        assert_eq!(derive_github_path(tmp.path()), None);
    }

    /// Why: a canonical identity string must round-trip through `rel_path` →
    /// `parse_owner_repo` unchanged, and must NOT drop `owner` (the URL parser
    /// bug this function exists to avoid).
    /// Test: itself.
    #[test]
    fn parse_owner_repo_roundtrips_rel_path() {
        let gp = parse_owner_repo("bobmatnyc/trusty-tools").expect("parsed");
        assert_eq!(gp.owner, "bobmatnyc");
        assert_eq!(gp.repo, "trusty-tools");
        assert_eq!(parse_owner_repo(&gp.rel_path()), Some(gp));
        // Nested group paths collapse to the final two segments.
        let nested = parse_owner_repo("acme/team/widget").unwrap();
        assert_eq!(nested.owner, "team");
        assert_eq!(nested.repo, "widget");
        // Mixed case / underscores are slugified, matching derive-time output.
        let slug = parse_owner_repo("Acme/Cool_App").unwrap();
        assert_eq!(slug.owner, "acme");
        assert_eq!(slug.repo, "cool-app");
    }

    /// Why: a single trailing segment (no owner) must still yield a two-segment
    /// identity via the `UNKNOWN_OWNER` sentinel, mirroring `parse_github_path`.
    /// Test: itself.
    #[test]
    fn parse_owner_repo_single_segment() {
        let gp = parse_owner_repo("repo").unwrap();
        assert_eq!(gp.owner, UNKNOWN_OWNER);
        assert_eq!(gp.repo, "repo");
    }

    /// Why: empty / slug-less inputs have no identity and must return `None`.
    /// Test: itself.
    #[test]
    fn parse_owner_repo_empty_returns_none() {
        assert_eq!(parse_owner_repo(""), None);
        assert_eq!(parse_owner_repo("   "), None);
        assert_eq!(parse_owner_repo("///"), None);
    }

    /// Why: `parse_remote_url` is the workspace's ONE git-remote-URL parser
    /// (#6657), so every shape its three call sites relied on is pinned in one
    /// table — including the case preservation `parse_github_path` does not
    /// offer, and the strict two-segment path that keeps a subgroup URL from
    /// fabricating an owner containing a slash.
    /// Test: itself.
    #[test]
    fn parse_remote_url_table() {
        for (url, host, owner, repo) in [
            (
                "https://github.com/bobmatnyc/Trusty-Tools.git",
                "github.com",
                "bobmatnyc",
                "Trusty-Tools",
            ),
            (
                "https://github.com/bobmatnyc/trusty-tools",
                "github.com",
                "bobmatnyc",
                "trusty-tools",
            ),
            (
                "https://user@github.com/acme/widget",
                "github.com",
                "acme",
                "widget",
            ),
            (
                "git@github.com:duettoresearch/pricing.git",
                "github.com",
                "duettoresearch",
                "pricing",
            ),
            (
                "ssh://git@github.com/acme/widget.git",
                "github.com",
                "acme",
                "widget",
            ),
            (
                "https://ghe.corp.example.com/Platform/Deploy_Tools.git",
                "ghe.corp.example.com",
                "Platform",
                "Deploy_Tools",
            ),
            (
                "git@ghe.corp.example.com:Platform/deploy.git",
                "ghe.corp.example.com",
                "Platform",
                "deploy",
            ),
            (
                "https://git.example.com:8443/owner/repo",
                "git.example.com:8443",
                "owner",
                "repo",
            ),
            (
                "https://github.com/acme/widget/",
                "github.com",
                "acme",
                "widget",
            ),
        ] {
            let parsed = parse_remote_url(url).unwrap_or_else(|e| panic!("`{url}`: {e}"));
            assert_eq!(parsed.host, host, "host of `{url}`");
            assert_eq!(parsed.owner, owner, "owner of `{url}`");
            assert_eq!(parsed.repo, repo, "repo of `{url}`");
            assert_eq!(parsed.owner_repo(), format!("{owner}/{repo}"));
        }

        for garbage in [
            "",
            "   ",
            "nonsense",
            "/var/log/trusty-mpm",
            "https://github.com/",
            "https://github.com/owner",
            "https://gitlab.com/group/subgroup/repo",
            "git@github.com:",
        ] {
            let err =
                parse_remote_url(garbage).expect_err("garbage must be refused, never guessed at");
            assert!(
                matches!(err, RemoteUrlError::Malformed { .. }),
                "`{garbage}` produced {err:?}"
            );
        }
    }

    /// Why: the I/O entry point must read a real `remote.origin.url` without
    /// slugifying it, and must resolve from a SUBDIRECTORY of the checkout —
    /// which is how the log drain identifies a project from its log directory.
    /// Test: itself (skips cleanly if `git` is unavailable on the runner).
    #[test]
    fn derive_remote_repo_reads_origin() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let dir = tmp.path();
        let git = |args: &[&str]| {
            Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        };
        if !git(&["init"]) {
            return;
        }
        let _ = git(&[
            "remote",
            "add",
            "origin",
            "git@github.com:duettoresearch/Pricing.git",
        ]);
        let parsed = derive_remote_repo(dir).expect("derived from origin");
        assert_eq!(parsed.owner, "duettoresearch");
        assert_eq!(parsed.repo, "Pricing", "the case the remote spells");

        let nested = dir.join("logs");
        std::fs::create_dir_all(&nested).expect("nested dir");
        assert_eq!(
            derive_remote_repo(&nested).expect("git walks up to the checkout"),
            parsed
        );
    }

    /// Why: a directory that is not a checkout must produce an error naming it,
    /// not a fabricated identity — the drain refuses to upload without one.
    /// Test: itself.
    #[test]
    fn derive_remote_repo_without_origin() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        match derive_remote_repo(tmp.path()) {
            Err(RemoteUrlError::NoOrigin { dir }) => {
                assert!(dir.contains(&*tmp.path().to_string_lossy()), "{dir}");
            }
            other => panic!("expected NoOrigin, got {other:?}"),
        }
    }

    /// Why: `parse_owner_repo` treats its input as a canonical `owner/repo`
    /// PATH, not a URL — feeding it an SSH (`git@host:owner/repo`) or HTTPS
    /// (`https://host/owner/repo`) remote must be rejected rather than silently
    /// mis-slugified into a nonsense identity (e.g. owner=`host:owner`).
    /// Test: itself.
    #[test]
    fn parse_owner_repo_rejects_url_shaped_input() {
        assert_eq!(
            parse_owner_repo("git@github.com:bobmatnyc/trusty-tools.git"),
            None
        );
        assert_eq!(
            parse_owner_repo("https://github.com/bobmatnyc/trusty-tools"),
            None
        );
        assert_eq!(parse_owner_repo("ssh://git@host/owner/repo"), None);
    }
}
