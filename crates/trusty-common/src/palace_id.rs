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
//! Length invariant (#2443): trusty-memory's `palace_create` rejects any slug
//! over [`PALACE_ID_MAX_LEN`] bytes, and derivation used to have no cap — a long
//! repo or directory name derived an id the daemon refused, so trusty-code's
//! turn recorder failed its ensure-palace step on every turn and never recorded
//! anything for that project. [`palace_id_is_valid`] now states the daemon's
//! rule once (trusty-memory's `validate_slug_format` calls it), and every level
//! of [`derive_palace_id`] runs its result through [`clamp_palace_id`], so the
//! two halves cannot disagree again.
//!
//! Test: `cargo test -p trusty-common --features unconditional-only --
//! palace_id::tests` exercises every precedence level and every git-URL variant
//! (SSH/HTTPS, with/without `.git`, trailing slashes, non-GitHub hosts,
//! owner-less paths, host:port URLs).
//!
//! [`derive_palace_id`]: crate::palace_id::derive_palace_id

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

/// Longest palace id trusty-memory's creation gate accepts.
///
/// Why: a palace id becomes a directory name under the data root and a
/// Unix-socket filename, so the daemon caps its length. Derivation carried no
/// cap at all, so a long repo or directory name produced an id the daemon
/// refused; trusty-code's turn recorder then failed its ensure-palace step on
/// every turn and stayed fail-open for that project (#2443). The number lives
/// beside the derivation that has to respect it.
/// What: `63` bytes.
/// Test: `derived_ids_always_pass_validation`, and trusty-memory's
/// `force_flag_rejects_unsafe_slugs`.
pub const PALACE_ID_MAX_LEN: usize = 63;

/// Does `slug` satisfy trusty-memory's palace-creation format gate?
///
/// Why (#2443): derivation and validation were two independent statements of
/// one rule, and they disagreed — the deriving side had no length cap while the
/// validating side rejected anything over [`PALACE_ID_MAX_LEN`]. This is the one
/// statement both sides read: trusty-memory's `validate_slug_format` calls it,
/// and every id [`derive_palace_id`] returns is built to satisfy it.
/// What: accepts `[a-z0-9][a-z0-9-]{0,62}` — 1 to [`PALACE_ID_MAX_LEN`] bytes of
/// lowercase ASCII letters, digits and hyphens, with a letter or digit first.
/// Everything else, the empty string included, is `false`.
/// Test: `palace_id_is_valid_accepts_and_rejects`,
/// `derived_ids_always_pass_validation`.
pub fn palace_id_is_valid(slug: &str) -> bool {
    if slug.is_empty() || slug.len() > PALACE_ID_MAX_LEN {
        return false;
    }
    slug.bytes()
        .enumerate()
        .all(|(i, b)| b.is_ascii_lowercase() || b.is_ascii_digit() || (i > 0 && b == b'-'))
}

/// Number of hex digits [`clamp_palace_id`] appends when it has to truncate.
const CLAMP_HASH_HEX: usize = 8;

/// Fit an already-slugified token inside [`PALACE_ID_MAX_LEN`].
///
/// Why: truncating alone maps two long project names that share a 54-byte
/// prefix onto one id, and one shared id is how two projects' prompts and
/// responses end up in the same palace (#5811). The hash suffix keeps distinct
/// inputs distinct.
/// What: returns `slug` unchanged when it already fits. Otherwise keeps the
/// leading bytes up to `PALACE_ID_MAX_LEN - CLAMP_HASH_HEX - 1`, trims trailing
/// hyphens off that prefix, and appends `-` plus [`CLAMP_HASH_HEX`] hex digits
/// derived from the WHOLE input — so the result is at most
/// [`PALACE_ID_MAX_LEN`] bytes and still matches [`palace_id_is_valid`]. The
/// prefix is built character by character, so a caller that passes non-ASCII
/// text gets a short id rather than a panic.
/// Test: `long_ids_are_clamped_to_the_limit`,
/// `clamped_ids_with_a_shared_prefix_stay_distinct`, `short_ids_pass_through`.
pub fn clamp_palace_id(slug: &str) -> String {
    if slug.len() <= PALACE_ID_MAX_LEN {
        return slug.to_string();
    }
    let budget = PALACE_ID_MAX_LEN - CLAMP_HASH_HEX - 1;
    let mut prefix = String::with_capacity(budget);
    for c in slug.chars() {
        if prefix.len() + c.len_utf8() > budget {
            break;
        }
        prefix.push(c);
    }
    while prefix.ends_with('-') {
        prefix.pop();
    }
    let suffix = format!("{:08x}", (fnv1a_64(slug.as_bytes()) >> 32) as u32);
    if prefix.is_empty() {
        suffix
    } else {
        format!("{prefix}-{suffix}")
    }
}

/// FNV-1a, 64-bit, over `bytes`.
///
/// Why: [`clamp_palace_id`]'s suffix becomes part of a project's palace id, so
/// it must be identical on every machine and every toolchain.
/// `std::collections::hash_map::DefaultHasher` is documented as unstable across
/// Rust releases, and a suffix that moved would silently re-home a project's
/// memory. FNV-1a is a fixed algorithm and needs no dependency.
/// What: the published 64-bit offset basis and prime, folded a byte at a time.
/// Test: `fnv1a_64_matches_published_vectors`.
fn fnv1a_64(bytes: &[u8]) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    bytes
        .iter()
        .fold(OFFSET_BASIS, |h, b| (h ^ u64::from(*b)).wrapping_mul(PRIME))
}

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
    let (owner_slug, repo_slug) = parse_owner_repo_slugs(url)?;
    // #2443: a long owner or repo name used to produce an id past the daemon's
    // 63-byte limit, which `palace_create` then rejected.
    Some(clamp_palace_id(&match owner_slug {
        Some(owner) => format!("{owner}-{repo_slug}"),
        None => repo_slug,
    }))
}

/// Parse a git remote URL into just the slugified `repo` segment (no owner).
///
/// Why: the claude-mpm-era palaces are named by the BARE repo name (e.g.
/// `trusty-tools`), NOT the `owner-repo` slug that [`owner_repo_from_git_remote`]
/// produces (`bobmatnyc-trusty-tools`). trusty-mpm's session-launch alias
/// registration (issue #1939) needs the bare-repo candidate to decide whether a
/// pre-existing bare palace should be aliased to the derived owner-repo palace.
/// Deriving it from the same remote (rather than string-splitting the owner-repo
/// slug on `-`, which is ambiguous because a repo name may itself contain `-`)
/// keeps the two identities consistent and unambiguous.
/// What: reuses the same host/path parsing as [`owner_repo_from_git_remote`] but
/// returns only the slugified final `repo` segment. Returns `None` when nothing
/// usable can be extracted (empty input, host-only URL).
/// Test: `repo_slug_ssh_github`, `repo_slug_https_with_owner`,
/// `repo_slug_repo_only`, `repo_slug_empty_returns_none`.
pub fn repo_slug_from_git_remote(url: &str) -> Option<String> {
    parse_owner_repo_slugs(url).map(|(_, repo)| clamp_palace_id(&repo))
}

/// Shared parse: extract the slugified `(owner, repo)` pair from a git remote.
///
/// Why: both [`owner_repo_from_git_remote`] (which joins owner+repo) and
/// [`repo_slug_from_git_remote`] (which keeps only repo) need identical host/path
/// isolation and slugification; factoring it here guarantees they can never
/// diverge on how a URL is parsed.
/// What: strips the scheme and host, trims a trailing `.git`/slashes, takes the
/// last two path segments as `(owner, repo)`, and slugifies each. Returns
/// `(Some(owner_slug), repo_slug)` for `owner/repo` URLs, `(None, repo_slug)`
/// when only a repo segment is present (or the owner slugifies to empty), and
/// `None` when no non-empty repo slug can be extracted.
/// Test: covered transitively by the `owner_repo_*` and `repo_slug_*` tests.
fn parse_owner_repo_slugs(url: &str) -> Option<(Option<String>, String)> {
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

    let owner_slug = owner.map(slugify_string).filter(|s| !s.is_empty());
    Some((owner_slug, repo_slug))
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

    // #2443: `Projects/<80-char-directory>` used to derive an id the daemon
    // refused, which left the turn recorder permanently fail-open.
    Some(clamp_palace_id(&match parent_slug {
        Some(parent) => format!("{parent}-{leaf_slug}"),
        None => leaf_slug,
    }))
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
/// unparseable remote, and a root-less path all at once). Every non-`None`
/// return satisfies [`palace_id_is_valid`] — each level runs its result through
/// [`clamp_palace_id`], so the daemon's creation gate can never reject an id
/// this function produced (#2443).
/// Test: `override_env_wins_over_git`, `git_used_when_no_override`,
/// `falls_back_to_parent_dir`, `all_empty_returns_none`,
/// `derived_ids_always_pass_validation`.
pub fn derive_palace_id(
    project_root: &Path,
    git_remote: Option<&str>,
    override_value: Option<&str>,
) -> Option<String> {
    // 1. Explicit override always wins (when it slugifies to a non-empty token).
    if let Some(slug) = override_value.map(slugify_string).filter(|s| !s.is_empty()) {
        return Some(clamp_palace_id(&slug));
    }

    // 2. Git owner/repo identity.
    if let Some(slug) = git_remote.and_then(owner_repo_from_git_remote) {
        return Some(slug);
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
    // repo_slug_from_git_remote — bare repo name (no owner)
    // -----------------------------------------------------------------------

    /// Why: the bare-repo alias candidate must drop the owner entirely so
    /// `git@github.com:bobmatnyc/trusty-tools.git` yields `trusty-tools`, the
    /// claude-mpm-era palace name (issue #1939).
    /// Test: itself.
    #[test]
    fn repo_slug_ssh_github() {
        assert_eq!(
            repo_slug_from_git_remote("git@github.com:bobmatnyc/trusty-tools.git").as_deref(),
            Some("trusty-tools")
        );
    }

    /// Why: an HTTPS remote with an owner must still reduce to just the repo
    /// segment, and a repo name containing `-` must survive intact (proving we
    /// do not string-split the owner-repo slug on `-`).
    /// Test: itself.
    #[test]
    fn repo_slug_https_with_owner() {
        assert_eq!(
            repo_slug_from_git_remote("https://github.com/bobmatnyc/trusty-tools").as_deref(),
            Some("trusty-tools")
        );
        assert_eq!(
            repo_slug_from_git_remote("https://gitlab.com/acme/team/cool-widget.git").as_deref(),
            Some("cool-widget")
        );
    }

    /// Why: an owner-less remote must yield the repo slug (parity with
    /// `owner_repo_from_git_remote`'s repo-only branch).
    /// Test: itself.
    #[test]
    fn repo_slug_repo_only() {
        assert_eq!(
            repo_slug_from_git_remote("git@host:repo.git").as_deref(),
            Some("repo")
        );
    }

    /// Why: unparseable input must return `None` so the caller skips alias
    /// registration entirely.
    /// Test: itself.
    #[test]
    fn repo_slug_empty_returns_none() {
        assert_eq!(repo_slug_from_git_remote(""), None);
        assert_eq!(repo_slug_from_git_remote("https://github.com/"), None);
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

    // -----------------------------------------------------------------------
    // #2443 — the derived id must satisfy the daemon's creation gate
    // -----------------------------------------------------------------------

    /// Why: this predicate is now the only statement of trusty-memory's rule,
    /// so each half of it needs a case on both sides of the line.
    /// Test: itself.
    #[test]
    fn palace_id_is_valid_accepts_and_rejects() {
        assert!(palace_id_is_valid("bobmatnyc-trusty-tools"));
        assert!(palace_id_is_valid("a"));
        assert!(palace_id_is_valid("9"));
        assert!(palace_id_is_valid(&"a".repeat(PALACE_ID_MAX_LEN)));

        assert!(!palace_id_is_valid(""));
        assert!(!palace_id_is_valid(&"a".repeat(PALACE_ID_MAX_LEN + 1)));
        assert!(!palace_id_is_valid("-leading-hyphen"));
        assert!(!palace_id_is_valid("UPPER"));
        assert!(!palace_id_is_valid("has_underscore"));
        assert!(!palace_id_is_valid("has space"));
        assert!(!palace_id_is_valid("../traversal"));
    }

    /// Why: the suffix becomes part of a project's palace id, so the hash has
    /// to be the published FNV-1a and not something that moves with the
    /// toolchain. These are the standard 64-bit vectors.
    /// Test: itself.
    #[test]
    fn fnv1a_64_matches_published_vectors() {
        assert_eq!(fnv1a_64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a_64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a_64(b"foobar"), 0x8594_4171_f739_67e8);
    }

    /// Why: an id that already fits must come back byte-identical — clamping
    /// must not re-home every existing project's palace.
    /// Test: itself.
    #[test]
    fn short_ids_pass_through() {
        assert_eq!(
            clamp_palace_id("bobmatnyc-trusty-tools"),
            "bobmatnyc-trusty-tools"
        );
        let exactly_at_limit = "a".repeat(PALACE_ID_MAX_LEN);
        assert_eq!(clamp_palace_id(&exactly_at_limit), exactly_at_limit);
    }

    /// Why: the defect itself — an over-long derived id was handed to
    /// `palace_create`, which rejected it, and the turn recorder went
    /// permanently fail-open for that project.
    /// Test: itself.
    #[test]
    fn long_ids_are_clamped_to_the_limit() {
        let long = "a".repeat(PALACE_ID_MAX_LEN + 1);
        let clamped = clamp_palace_id(&long);
        assert!(
            palace_id_is_valid(&clamped),
            "clamped id must pass the daemon gate, got {clamped:?}"
        );
        // A prefix ending in a hyphen must not leave one dangling before the
        // suffix separator.
        let hyphen_at_the_cut = format!("{}{}", "b".repeat(53), "-".repeat(40));
        let clamped = clamp_palace_id(&hyphen_at_the_cut);
        assert!(palace_id_is_valid(&clamped), "got {clamped:?}");
        assert!(!clamped.contains("--"), "got {clamped:?}");
    }

    /// Why: plain truncation would map two long projects sharing a prefix onto
    /// one palace, and one shared id is how two projects' turns end up in the
    /// same place (#5811).
    /// Test: itself.
    #[test]
    fn clamped_ids_with_a_shared_prefix_stay_distinct() {
        let shared = "acme-organisation-with-a-very-long-name-indeed-limited-";
        let a = clamp_palace_id(&format!("{shared}first-service"));
        let b = clamp_palace_id(&format!("{shared}second-service"));
        assert_ne!(a, b, "distinct projects must not share a palace id");
        assert!(palace_id_is_valid(&a), "got {a:?}");
        assert!(palace_id_is_valid(&b), "got {b:?}");
        // Deterministic across calls — a project's palace must not move.
        assert_eq!(a, clamp_palace_id(&format!("{shared}first-service")));
    }

    /// Why (#2443): the round-trip the bug family wants — every id derivation
    /// can produce, over every precedence level, must be one the daemon's
    /// `validate_slug_format` accepts. Written as a corpus sweep rather than a
    /// `proptest` case because this workspace carries no property-testing
    /// dependency and this rule needs no shrinking to be actionable.
    /// Test: itself.
    #[test]
    fn derived_ids_always_pass_validation() {
        let names = [
            "trusty-tools",
            "Trusty_Tools",
            "a",
            "9lives",
            "My Cool Project",
            "  --weird__name--  ",
            "project.git",
            &"very-long-repository-name-that-nobody-would-type-on-purpose".repeat(3),
            &"x".repeat(200),
            &"a-".repeat(80),
            "проект-кириллица",
            "漢字-project",
            "café-service",
        ];
        let remote_shapes: [fn(&str) -> String; 4] = [
            |n| format!("git@github.com:{n}/{n}.git"),
            |n| format!("https://github.com/some-owner/{n}"),
            |n| format!("https://git.company.com:8080/{n}/{n}.git"),
            |n| format!("git@host:{n}.git"),
        ];

        for name in names {
            let root = PathBuf::from(format!("/Users/bob/{name}/{name}"));

            // Level 3 — parent/dir.
            if let Some(id) = derive_palace_id(&root, None, None) {
                assert!(palace_id_is_valid(&id), "parent/dir {name:?} -> {id:?}");
            }
            // Level 1 — explicit override.
            if let Some(id) = derive_palace_id(&root, None, Some(name)) {
                assert!(palace_id_is_valid(&id), "override {name:?} -> {id:?}");
            }
            // Level 2 — git owner/repo, in every URL shape.
            for shape in remote_shapes {
                let remote = shape(name);
                if let Some(id) = derive_palace_id(&root, Some(&remote), None) {
                    assert!(palace_id_is_valid(&id), "remote {remote:?} -> {id:?}");
                }
                if let Some(id) = owner_repo_from_git_remote(&remote) {
                    assert!(palace_id_is_valid(&id), "owner_repo {remote:?} -> {id:?}");
                }
                if let Some(id) = repo_slug_from_git_remote(&remote) {
                    assert!(palace_id_is_valid(&id), "repo_slug {remote:?} -> {id:?}");
                }
            }
        }
    }
}
