//! Positional-argument resolution for `tm register` (#4912).
//!
//! Why: `tm register` used to be `<ALIAS> <URL>` with both required. #4912 makes
//! the URL come first and the alias optional. Two hazards come with that, and
//! both are handled here rather than downstream.
//!
//! 1. Swapping positional order alone would break existing `tm register
//!    <alias> <url>` invocations. A URL is mechanically detectable, so
//!    [`resolve_register_args`] detects which positional is the URL and routes
//!    accordingly, keeping the legacy order working.
//! 2. Making the alias optional means a single argument is now interpreted as a
//!    URL. Anything accepted there gets registered, so the URL test has to be
//!    strict enough to reject what is NOT a clone-able URL — `gh`-style
//!    `owner/repo` shorthand most of all, since it is the likeliest thing a
//!    user types and the deriver reads `owner` as the host
//!    (`bobmatnyc/trusty-tools` → alias `trusty-tools`, an unclonable URL, exit
//!    0, and no collision with the later correct registration).
//!    [`looks_like_url`] therefore requires a scheme, a `git@` prefix, a local
//!    path, or a host-shaped first segment — a `/` alone is not enough.
//!
//! This module does NOT expand `owner/repo` into a GitHub URL. That is a
//! product decision about defaulting to GitHub and is out of scope for #4912.
//!
//! What: [`resolve_register_args`] maps the raw `(first, second?)` positionals
//! onto a `(alias, url)` pair, rejecting URLs that cannot name a repo and
//! deriving the alias when only one positional was given. Derivation delegates
//! to [`trusty_common::palace_id::owner_repo_from_git_remote`] — the workspace's
//! single git-URL→`owner-repo` parser. The local [`path_segments`] is a
//! *validator*, not a second deriver: it exists to reject inputs the shared
//! parser would happily turn into nonsense, because that parser is a
//! best-effort palace-ID deriver whose leniency other callers depend on.
//! Test: `register_args_tests.rs`.

/// Path segments that mean the URL points INTO a repo rather than AT one.
///
/// Why: making the alias optional invites pasting a browser URL, and the shared
/// deriver takes the last two path segments — so `…/owner/repo/tree/main`
/// derives `tree-main` and `…/owner/repo/pull/4914` derives `pull-4914`, both
/// silently. These are the GitHub/GitLab web-UI path words that appear directly
/// after `owner/repo`.
/// What: matched only at path index ≥ 2 (after `owner/repo`) and only when a
/// segment follows, so a repo legitimately NAMED `tree` still registers.
/// Test: `browser_paste_shapes_are_rejected`, `repo_named_like_a_web_path_is_ok`.
const NON_REPO_SEGMENTS: &[&str] = &[
    "tree", "blob", "pull", "pulls", "issues", "commit", "commits", "compare", "releases",
    "actions", "wiki", "blame", "raw",
];

/// Resolve the `tm register` positionals into `(alias, url)`.
///
/// Why: see the module docs — accepting both positional orders keeps the #4912
/// swap from breaking existing invocations, and the strict URL test keeps a
/// single non-URL argument from being registered as one.
/// What: with two positionals, whichever one [`looks_like_url`] accepts becomes
/// the URL and the other becomes the alias; if both or neither look like a URL,
/// this errors rather than guessing. With one positional it must be a URL, and
/// the alias is derived as `owner-repo`. The returned URL has any query string
/// and fragment stripped — neither is ever part of a clone URL, and leaving them
/// on both corrupts the derived alias and stores an unclonable URL.
///
/// Alias derivation falls back to the bare repo slug when the URL exposes no
/// owner segment (`https://example.com/repo` → `repo`); a URL with no repo path
/// at all is rejected by [`validated_url`] first.
/// Test: `two_args_url_first`, `two_args_legacy_alias_first`, `one_arg_derives`,
/// `one_arg_shorthand_is_rejected`, `one_arg_non_url_errors`, `two_urls_error`,
/// `two_non_urls_error`, `derives_from_every_url_shape`,
/// `no_owner_falls_back_to_repo`, `host_only_url_errors`,
/// `browser_paste_shapes_are_rejected`, `query_and_fragment_are_stripped`.
pub(crate) fn resolve_register_args(
    first: &str,
    second: Option<&str>,
) -> anyhow::Result<(String, String)> {
    let first = first.trim();
    match second.map(str::trim) {
        Some(second) => match (looks_like_url(first), looks_like_url(second)) {
            // #4912: the new order — `tm register <URL> [ALIAS]`.
            (true, false) => Ok((second.to_string(), validated_url(first)?)),
            // #4912: the legacy order — `tm register <ALIAS> <URL>` still works.
            (false, true) => Ok((first.to_string(), validated_url(second)?)),
            (true, true) => Err(anyhow::anyhow!(
                "both arguments look like URLs ('{first}' and '{second}') — \
                 refusing to guess which is the alias. Usage: tm register <url> [alias]"
            )),
            (false, false) => Err(anyhow::anyhow!(
                "neither argument looks like a repository URL ('{first}' and '{second}'). \
                 Usage: tm register <url> [alias]"
            )),
        },
        None => {
            if !looks_like_url(first) {
                return Err(not_a_url_error(first));
            }
            let url = validated_url(first)?;
            let alias = derive_alias(&url)?;
            Ok((alias, url))
        }
    }
}

/// Normalise a URL and reject the shapes that cannot name a repository.
///
/// Why: everything that reaches here gets stored and later handed to
/// `git clone`, and — when no alias was given — turned into one. The shared
/// deriver never rejects; it returns a best-effort slug for any string. So the
/// rejecting happens here, at the CLI boundary, where a bad input still has a
/// user in front of it.
/// What: strips the fragment and query string, then errors when the URL has no
/// host-relative path (`https://example.com`, with or without a trailing slash)
/// or when it points into a repo's web UI rather than at the repo
/// (`…/owner/repo/tree/main`). Returns the normalised URL otherwise.
/// Test: `host_only_url_errors`, `browser_paste_shapes_are_rejected`,
/// `query_and_fragment_are_stripped`, `repo_named_like_a_web_path_is_ok`.
fn validated_url(url: &str) -> anyhow::Result<String> {
    // A `?query` or `#fragment` is never part of a clone URL, and both corrupt
    // the derived alias (`…/o/r?tab=readme` → `o-rtabreadme`).
    let normalised = url
        .split('#')
        .next()
        .unwrap_or(url)
        .split('?')
        .next()
        .unwrap_or(url);

    let segments = path_segments(normalised);
    if segments.is_empty() {
        return Err(anyhow::anyhow!(
            "'{url}' has no owner/repo path — it names a host, not a repository. \
             Pass a full repository URL, e.g. https://github.com/<owner>/<repo>"
        ));
    }

    // A web-UI path word after `owner/repo`, with something following it, means
    // this is a browser paste rather than a clone URL. The final segment is
    // exempt so a repo actually NAMED `tree` still registers.
    let interior = segments.len().saturating_sub(3);
    if let Some(bad) = segments
        .iter()
        .skip(2)
        .take(interior)
        .find(|s| NON_REPO_SEGMENTS.contains(s))
    {
        return Err(anyhow::anyhow!(
            "'{url}' points inside a repository ('/{bad}/…'), not at it. \
             Pass the repository root, e.g. https://github.com/<owner>/<repo>"
        ));
    }

    Ok(normalised.to_string())
}

/// Derive the default `owner-repo` alias from a repository URL.
///
/// Why: `tm register <url>` with no alias needs a deterministic default, and it
/// must be the SAME slug the rest of the workspace already derives for that repo
/// (palace IDs, search index IDs) — so this delegates to trusty-common instead
/// of adding a second parser.
///
/// 🔴 The separator is a HYPHEN, not a slash, and that is deliberate: an alias
/// becomes a path segment wherever it is consumed (`<root>/projects/<alias>`,
/// socket names), so a literal `/` would nest into the filesystem. Do not
/// "fix" this to `owner/repo`. See `trusty_common::palace_id` module docs for
/// the same storage-safety invariant.
///
/// What: returns `owner-repo`, or the bare repo slug when the URL has no owner
/// segment. Callers pass a URL already through [`validated_url`]; the error here
/// is the last-resort guard for a slug the shared parser still refuses.
/// Test: `derives_from_every_url_shape`, `no_owner_falls_back_to_repo`.
fn derive_alias(url: &str) -> anyhow::Result<String> {
    trusty_common::palace_id::owner_repo_from_git_remote(url).ok_or_else(|| {
        anyhow::anyhow!(
            "cannot derive an alias from '{url}' — no owner/repo path found. \
             Pass one explicitly: tm register {url} <alias>"
        )
    })
}

/// Decide whether a positional is a repository URL rather than an alias.
///
/// Why: this test gates two things — which positional is the URL, and whether a
/// lone argument may be treated as one at all. The second is the sharper edge:
/// `contains('/')` alone accepts `gh`-style `owner/repo` shorthand, which the
/// deriver then reads as `host/repo`, registering an unclonable URL under the
/// wrong alias with exit 0. So a `/` is necessary but not sufficient.
/// What: true when the string carries a scheme (`://`), starts with `git@`, is a
/// local clone path (`/…` or `~/…`), or has a host-shaped first segment — one
/// containing a `.` or equal to `localhost` — followed by a non-empty path.
/// Disjointness from aliases still holds: a valid alias matches
/// `^[a-z0-9][a-z0-9._-]*$` (`validate_alias` in `core::standalone::registry`),
/// so it can never contain the `/` or `:` this requires.
/// Test: `looks_like_url_accepts_url_shapes`, `looks_like_url_rejects_aliases`,
/// `looks_like_url_rejects_gh_shorthand`.
fn looks_like_url(s: &str) -> bool {
    if s.contains("://") || s.starts_with("git@") {
        return true;
    }
    // Local clone sources — an absolute or home-relative path to a bare repo.
    if s.starts_with('/') || s.starts_with("~/") {
        return true;
    }
    // Scheme-less `host/owner/repo`: the first segment must look like a host,
    // or this is `owner/repo` shorthand and NOT a URL (#4912 review).
    let Some((first, rest)) = s.split_once(['/', ':']) else {
        return false;
    };
    !rest.is_empty() && (first.contains('.') || first == "localhost")
}

/// Build the error for a lone positional that is not a URL.
///
/// Why: the message is the whole remedy — the user typed something plausible
/// (`owner/repo`) and needs to be shown the full form, not just told it failed.
/// What: names the rejected string and shows both full-URL forms.
/// Test: `one_arg_shorthand_is_rejected`, `one_arg_non_url_errors`.
fn not_a_url_error(s: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "'{s}' is not a repository URL. Pass the full URL, \
         e.g. https://github.com/<owner>/<repo> or git@github.com:<owner>/<repo>.git \
         (usage: tm register <url> [alias])"
    )
}

/// Split a URL into its host-relative path segments.
///
/// Why: [`validated_url`] needs to know whether a path exists at all and what is
/// in it. trusty-common's equivalent is private and, deliberately, never reports
/// "nothing here" — it falls back rather than rejects. This is a validator for
/// the CLI boundary, kept small and pinned by
/// `path_segments_matches_every_url_shape` so it cannot drift from the deriver
/// on the shapes both see.
/// What: strips the scheme, an optional `user@` credential, and the host (with
/// its `:port` or scp-syntax `:`), then splits the remainder on `/` dropping
/// empties. A leading pure-digit segment is dropped as a port ONLY when the host
/// was terminated by `:` — otherwise `github.com/123/repo` would lose its owner.
/// This mirrors the port-vs-scp rule in trusty-common's `host_relative_path`, so
/// a numeric first segment resolves the same way in both.
/// Test: `path_segments_matches_every_url_shape`, `path_segments_host_only`,
/// `path_segments_keeps_numeric_owner`.
fn path_segments(url: &str) -> Vec<&str> {
    let after_scheme = match url.find("://") {
        Some(i) => &url[i + 3..],
        None => url,
    };
    let after_creds = match after_scheme.find('@') {
        Some(i) => &after_scheme[i + 1..],
        None => after_scheme,
    };
    let host_end = after_creds.find(['/', ':']).unwrap_or(after_creds.len());
    let host_ended_with_colon = after_creds.as_bytes().get(host_end) == Some(&b':');
    let after_host = after_creds[host_end..].trim_start_matches([':', '/']);

    let mut segments: Vec<&str> = after_host.split('/').filter(|s| !s.is_empty()).collect();
    // `host:8080/owner/repo` leaves the port as the first segment.
    if host_ended_with_colon
        && segments
            .first()
            .is_some_and(|s| s.bytes().all(|b| b.is_ascii_digit()))
    {
        segments.remove(0);
    }
    segments
}

#[cfg(test)]
#[path = "register_args_tests.rs"]
mod tests;
