//! Positional-argument resolution for `tm register` (#4912).
//!
//! Why: `tm register` used to be `<ALIAS> <URL>` with both required. #4912 makes
//! the repo come first and the alias optional, and makes `owner/repo` the
//! primary way to name a repo. Three things have to be right at once.
//!
//! 1. Swapping positional order alone would break existing `tm register
//!    <alias> <url>` invocations, so [`resolve_register_args`] detects which
//!    positional names the repo and routes accordingly.
//! 2. `owner/repo` IS the identifier scheme, with GitHub assumed — the same
//!    assumption `tm launch` already makes when it gates on `is_github_remote`
//!    and refuses non-GitHub remotes. `tm register bobmatnyc/trusty-tools`
//!    resolves to `https://github.com/bobmatnyc/trusty-tools`. Non-GitHub
//!    shorthand is deferred, not refused on the merits: a full non-GitHub URL
//!    still registers.
//! 3. Making the repo argument optional-alias'd means whatever is accepted here
//!    gets stored and later cloned. So the classification is explicit rather
//!    than "contains a slash": that test accepted `owner/repo` as a URL whose
//!    HOST was `owner`, registering an unclonable string under alias
//!    `trusty-tools` with exit 0 — and, because the alias differed from the
//!    canonical `bobmatnyc-trusty-tools`, the later correct registration did not
//!    even collide.
//!
//! What: [`classify`] sorts a positional into [`Positional`] — full URL, GitHub
//! shorthand, filesystem path, or neither — and [`resolve_register_args`] maps
//! the `(first, second?)` pair onto `(alias, url)`. Alias derivation delegates to
//! [`trusty_common::palace_id::owner_repo_from_git_remote`], the workspace's
//! single git-URL→`owner-repo` parser. The local [`path_segments`] is a
//! *validator*, not a second deriver: it rejects inputs the shared parser would
//! happily turn into nonsense, because that parser is a best-effort palace-ID
//! deriver whose leniency other callers depend on.
//! Test: `register_args_tests.rs`.

/// Host assumed for `owner/repo` shorthand, and the one host known to be flat.
///
/// Why: #4912 — GitHub is assumed for shorthand, matching `tm launch`'s existing
/// `is_github_remote` gate. Other hosts need a full URL for now. The same fact
/// does double duty in [`non_repo_segment`]: GitHub has no nested groups, so any
/// path past `owner/repo` on this host is a web-UI path.
const GITHUB_HOST: &str = "github.com";

/// Path segments that mean a URL points INTO a repo rather than AT one.
///
/// Why: an optional alias invites pasting a browser URL, and the shared deriver
/// takes the last two path segments — so `…/owner/repo/tree/main` derives
/// `tree-main` and `…/owner/repo/pull/4914` derives `pull-4914`, both silently.
/// What: used by [`non_repo_segment`] for hosts that may nest paths legitimately.
/// Test: `browser_paste_shapes_are_rejected`, `repo_named_like_a_web_path_is_ok`.
const NON_REPO_SEGMENTS: &[&str] = &[
    "tree", "blob", "pull", "pulls", "issues", "commit", "commits", "compare", "releases",
    "actions", "wiki", "blame", "raw",
];

/// What a single `tm register` positional names.
///
/// Why: the pre-review code asked one question — "does it contain a `/`?" — and
/// that conflated three different things. Naming the cases makes each one's
/// outcome a deliberate choice instead of a fallthrough.
/// What: `Url` parses as today; `Shorthand` resolves against [`SHORTHAND_HOST`];
/// `RelativePath` is refused because it is resolved against the process cwd but
/// cloned later from elsewhere; `Other` is either the alias positional or
/// garbage, decided by the caller from how many positionals there were.
/// Test: `classify_sorts_every_shape`.
#[derive(Debug, PartialEq, Eq)]
enum Positional<'a> {
    /// A full clone URL, or an absolute/home-relative local repo path.
    Url(&'a str),
    /// GitHub `owner/repo` shorthand — the primary form (#4912).
    Shorthand { owner: &'a str, repo: &'a str },
    /// Anything starting with `.` (`./x`, `../x`, `.hidden/x`) — a path, and
    /// explicitly NOT shorthand.
    RelativePath,
    /// Not a repo reference: an alias, or something malformed.
    Other,
}

/// Sort a positional into the case that decides its outcome.
///
/// Why: see [`Positional`]. The order of the arms is load-bearing — the
/// filesystem-path guards must run BEFORE the host-shape test, or a leading dot
/// matches "first segment contains a dot" and is read as a host.
/// What: a scheme, a `git@` prefix, or a leading `/`/`~/` is a `Url`; a leading
/// `.` is a `RelativePath`; a host-shaped first segment (containing a `.`, or
/// `localhost`) with a non-empty remainder is a `Url`; exactly two
/// `[A-Za-z0-9._-]` segments with no `:` are `Shorthand`; everything else is
/// `Other`.
///
/// A GitHub owner cannot contain a `.`, so "first segment has a dot" cleanly
/// separates `github.com/owner/repo` from `owner/repo` without a host list.
/// Test: `classify_sorts_every_shape`, `looks_like_repo_rejects_aliases`,
/// `relative_paths_are_never_shorthand`.
fn classify(s: &str) -> Positional<'_> {
    if s.contains("://") || s.starts_with("git@") {
        return Positional::Url(s);
    }
    // Paths first: any leading `.` would otherwise match the host-shape test
    // below on its own dot — `./repo` splits to `first = "."` and `.hidden/repo`
    // to `first = ".hidden"`. Neither is a host: no hostname may begin with a
    // dot, and no GitHub owner may contain one.
    if s.starts_with('.') {
        return Positional::RelativePath;
    }
    // Absolute and home-relative paths are unambiguous local clone sources.
    if s.starts_with('/') || s.starts_with("~/") {
        return Positional::Url(s);
    }

    let Some((first, rest)) = s.split_once(['/', ':']) else {
        return Positional::Other;
    };
    if rest.is_empty() {
        return Positional::Other;
    }
    if first.contains('.') || first == "localhost" {
        return Positional::Url(s);
    }

    // #4912: `owner/repo` with GitHub assumed. Exactly two segments, and no `:`
    // anywhere (a `:` means an scp-style host or a port, never shorthand).
    if s.contains(':') {
        return Positional::Other;
    }
    match s.split('/').collect::<Vec<_>>().as_slice() {
        [owner, repo] if is_name_segment(owner) && is_name_segment(repo) => {
            Positional::Shorthand { owner, repo }
        }
        _ => Positional::Other,
    }
}

/// True when a shorthand segment is a plausible GitHub owner or repo name.
///
/// Why: shorthand is resolved into a URL that gets cloned, so a segment with a
/// space or a shell metacharacter must not become one.
/// What: non-empty and `[A-Za-z0-9._-]` only.
/// Test: `classify_sorts_every_shape`.
fn is_name_segment(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// True when a positional names a repo (either form) rather than an alias.
///
/// Why: the two-positional path only needs "which one is the repo", and
/// disjointness holds — a valid alias matches `^[a-z0-9][a-z0-9._-]*$`
/// (`validate_alias` in `core::standalone::registry`), so it can never contain
/// the `/` or `:` every repo form requires.
/// Test: `looks_like_repo_rejects_aliases`, `two_args_legacy_alias_first`.
fn looks_like_repo(s: &str) -> bool {
    matches!(
        classify(s),
        Positional::Url(_) | Positional::Shorthand { .. }
    )
}

/// Resolve the `tm register` positionals into `(alias, url)`.
///
/// Why: see the module docs. Accepting both positional orders keeps the #4912
/// swap from breaking existing invocations; the explicit classification keeps a
/// non-repo argument from being registered as one.
/// What: with two positionals, whichever one [`looks_like_repo`] accepts becomes
/// the repo and the other becomes the alias; both or neither is an error rather
/// than a guess. With one positional it must name a repo, and the alias is
/// derived as `owner-repo`. The returned URL is fully resolved — shorthand
/// becomes a `https://github.com/…` URL, and any `?query`/`#fragment` is
/// stripped, because this string is what `tm load` later clones.
/// Test: `shorthand_resolves_to_github`, `two_args_url_first`,
/// `two_args_legacy_alias_first`, `one_arg_derives`, `two_urls_error`,
/// `two_non_urls_error`, `derives_from_every_url_shape`,
/// `no_owner_falls_back_to_repo`, `host_only_url_errors`,
/// `browser_paste_shapes_are_rejected`, `query_and_fragment_are_stripped`,
/// `relative_paths_are_never_shorthand`.
pub(crate) fn resolve_register_args(
    first: &str,
    second: Option<&str>,
) -> anyhow::Result<(String, String)> {
    let first = first.trim();
    match second.map(str::trim) {
        Some(second) => match (looks_like_repo(first), looks_like_repo(second)) {
            // #4912: the new order — `tm register <repo> [ALIAS]`.
            (true, false) => Ok((second.to_string(), resolved_url(first)?)),
            // #4912: the legacy order — `tm register <ALIAS> <url>` still works.
            (false, true) => Ok((first.to_string(), resolved_url(second)?)),
            (true, true) => Err(anyhow::anyhow!(
                "both arguments name a repository ('{first}' and '{second}') — \
                 refusing to guess which is the alias. Usage: tm register <owner/repo> [alias]"
            )),
            (false, false) => Err(anyhow::anyhow!(
                "neither argument names a repository ('{first}' and '{second}'). \
                 Usage: tm register <owner/repo> [alias]"
            )),
        },
        None => {
            if !looks_like_repo(first) {
                return Err(rejection(first));
            }
            let url = resolved_url(first)?;
            let alias = derive_alias(&url)?;
            Ok((alias, url))
        }
    }
}

/// Build the error for a positional that does not name a repository.
///
/// Why: the message is the remedy. A relative path and a bare word fail for
/// different reasons and need different advice.
/// Test: `relative_paths_are_never_shorthand`, `one_arg_non_repo_errors`,
/// `two_non_urls_error`.
fn rejection(s: &str) -> anyhow::Error {
    match classify(s) {
        Positional::RelativePath => anyhow::anyhow!(
            "'{s}' is a relative path, not a repository. It would be resolved \
             against the current directory but cloned from elsewhere. Pass \
             <owner>/<repo>, a full URL, or an absolute path"
        ),
        _ => anyhow::anyhow!(
            "'{s}' does not name a repository. Pass <owner>/<repo> (GitHub is \
             assumed), or a full URL such as https://github.com/<owner>/<repo> \
             or git@github.com:<owner>/<repo>.git"
        ),
    }
}

/// Resolve a positional into the URL that will be stored and cloned.
///
/// Why: everything that reaches the registry is later handed to `git clone`,
/// and — when no alias was given — turned into one. The shared deriver never
/// rejects; it returns a best-effort slug for any string. So the rejecting
/// happens here, at the CLI boundary, where a bad input still has a user in
/// front of it.
/// What: shorthand becomes `https://github.com/<owner>/<repo>`; a leading `~/`
/// is expanded to a real path. A full URL has its fragment and query string
/// stripped, then errors when it has no host-relative path
/// (`https://example.com`, with or without a trailing slash) or points into a
/// repo's web UI rather than at the repo. Finally a scheme-less host gains
/// `https://`, because `git clone github.com/o/r` fails.
/// Test: `shorthand_resolves_to_github`, `host_only_url_errors`,
/// `browser_paste_shapes_are_rejected`, `github_tab_urls_are_rejected`,
/// `query_and_fragment_are_stripped`, `scheme_less_host_gains_https`,
/// `home_relative_path_is_expanded`.
fn resolved_url(s: &str) -> anyhow::Result<String> {
    let url: String = match classify(s) {
        // #4912: GitHub is assumed for shorthand; the stored value must be
        // clone-able, so the host goes on here rather than at clone time.
        Positional::Shorthand { owner, repo } => {
            return Ok(format!("https://{GITHUB_HOST}/{owner}/{repo}"));
        }
        // #4912: `clone_repo` (`core::standalone::load`) runs `git` with no
        // shell, so a literal `~` never expands and the clone fails. Expand it
        // here, through the same helper that resolves the managed root, which
        // leaves the input equivalent to the absolute path it names.
        Positional::Url(url) if url == "~" || url.starts_with("~/") => {
            crate::commands::managed_root::expand_tilde(url)?
                .to_string_lossy()
                .into_owned()
        }
        // A `?query` or `#fragment` is never part of a clone URL, and both
        // corrupt the derived alias (`…/o/r?tab=readme` → `o-rtabreadme`).
        Positional::Url(url) => url.split(['#', '?']).next().unwrap_or(url).to_string(),
        _ => return Err(rejection(s)),
    };

    let segments = path_segments(&url);
    if segments.is_empty() {
        return Err(anyhow::anyhow!(
            "'{s}' has no owner/repo path — it names a host, not a repository. \
             Pass <owner>/<repo>, or a full repository URL"
        ));
    }
    if let Some(bad) = non_repo_segment(&url, &segments) {
        return Err(anyhow::anyhow!(
            "'{s}' points inside a repository ('/{bad}/…'), not at it. \
             Pass the repository root, e.g. <owner>/<repo>"
        ));
    }

    // #4912: `github.com/owner/repo` classifies as a URL on its host-shaped
    // first segment, but git needs a scheme — stored verbatim it failed at clone
    // time with `repository '…' does not exist`, long after exit 0.
    if !url.contains("://") && !url.starts_with("git@") && !url.starts_with('/') {
        return Ok(format!("https://{url}"));
    }
    Ok(url)
}

/// Find the path segment proving a URL points INTO a repo rather than at it.
///
/// Why: two rules, because hosts differ. On GitHub any path past `owner/repo` is
/// a web-UI path — GitHub has no nested groups — and that is the rule that
/// catches the most-pasted shape of all, `…/owner/repo/issues`, whose keyword is
/// the LAST segment and so escapes the interior rule. Other hosts nest
/// legitimately (GitLab subgroups), so there a [`NON_REPO_SEGMENTS`] word counts
/// only in an interior position, which leaves a repo actually NAMED `tree`
/// registrable. A local path has no host and so gets neither rule.
/// What: returns the offending segment, or `None` when the URL names a repo root.
/// Test: `github_tab_urls_are_rejected`, `browser_paste_shapes_are_rejected`,
/// `repo_named_like_a_web_path_is_ok`, `local_paths_skip_the_web_path_rule`.
fn non_repo_segment<'a>(url: &str, segments: &[&'a str]) -> Option<&'a str> {
    let (host, _, _) = split_host(url);
    // #4912: a local path has no host, so it has no web UI either and none of
    // these words mean anything in it. Without this, `/Users/me/actions/repo.git`
    // and `~/tree/repo.git` (expanded to an absolute path above, before this
    // runs) were refused as browser pastes — a shape that registered fine before
    // #4912, with an error naming a remedy that does not apply.
    if host.is_empty() {
        return None;
    }
    if host.eq_ignore_ascii_case(GITHUB_HOST) && segments.len() > 2 {
        return Some(segments[2]);
    }
    let interior = segments.len().saturating_sub(3);
    segments
        .iter()
        .skip(2)
        .take(interior)
        .find(|seg| NON_REPO_SEGMENTS.contains(seg))
        .copied()
}

/// Derive the default `owner-repo` alias from a resolved repository URL.
///
/// Why: `tm register <repo>` with no alias needs a deterministic default, and it
/// must be the SAME slug the rest of the workspace derives for that repo (palace
/// IDs, search index IDs) — so this delegates to trusty-common rather than
/// adding a second parser. Shorthand resolves to a URL first, so both input
/// forms derive through one path and cannot disagree.
///
/// 🔴 The separator is a HYPHEN, not a slash, and that is deliberate: an alias
/// becomes a path segment wherever it is consumed (`<root>/projects/<alias>`,
/// socket names), so a literal `/` would nest into the filesystem. Do not "fix"
/// this to `owner/repo`. See `trusty_common::palace_id` module docs for the same
/// storage-safety invariant.
///
/// What: returns `owner-repo`, or the bare repo slug when the URL has no owner
/// segment. Callers pass a URL already through [`resolved_url`]; the error here
/// is the last-resort guard for a slug the shared parser still refuses.
/// Test: `derives_from_every_url_shape`, `no_owner_falls_back_to_repo`,
/// `shorthand_resolves_to_github`.
fn derive_alias(url: &str) -> anyhow::Result<String> {
    trusty_common::palace_id::owner_repo_from_git_remote(url).ok_or_else(|| {
        anyhow::anyhow!(
            "cannot derive an alias from '{url}' — no owner/repo path found. \
             Pass one explicitly: tm register {url} <alias>"
        )
    })
}

/// Split a URL into its host-relative path segments.
///
/// Why: [`resolved_url`] needs to know whether a path exists at all and what is
/// in it. trusty-common's equivalent is private and, deliberately, never reports
/// "nothing here" — it falls back rather than rejects. This is a validator for
/// the CLI boundary, kept small and pinned by
/// `path_segments_matches_every_url_shape` so it cannot drift from the deriver
/// on the shapes both see.
/// What: strips the scheme, an optional `user@` credential, and the host (with
/// its `:port` or scp-syntax `:`), then splits the remainder on `/` dropping
/// empties. A leading pure-digit segment is dropped as a port ONLY when the host
/// was terminated by `:` — otherwise `github.com/123/repo` would lose its owner,
/// which matters now that `123/repo` is valid shorthand. This mirrors the
/// port-vs-scp rule in trusty-common's `host_relative_path`.
/// Test: `path_segments_matches_every_url_shape`, `path_segments_host_only`,
/// `path_segments_keeps_numeric_owner`.
fn path_segments(url: &str) -> Vec<&str> {
    let (_, rest, host_ended_with_colon) = split_host(url);
    let after_host = rest.trim_start_matches([':', '/']);

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

/// Split a URL into its host and the remainder that follows it.
///
/// Why: [`non_repo_segment`] and [`path_segments`] must agree on where the host
/// ends, or the GitHub rule would be applied against the wrong segments.
/// What: drops the scheme and any `user@` credential, then cuts at the first `/`
/// or `:`. The returned flag reports whether that terminator was a `:`, which is
/// what tells a `:port` from scp-syntax further down.
/// Test: `path_segments_matches_every_url_shape`, `github_tab_urls_are_rejected`.
fn split_host(url: &str) -> (&str, &str, bool) {
    let after_scheme = match url.find("://") {
        Some(i) => &url[i + 3..],
        None => url,
    };
    let after_creds = match after_scheme.find('@') {
        Some(i) => &after_scheme[i + 1..],
        None => after_scheme,
    };
    let host_end = after_creds.find(['/', ':']).unwrap_or(after_creds.len());
    let ended_with_colon = after_creds.as_bytes().get(host_end) == Some(&b':');
    (
        &after_creds[..host_end],
        &after_creds[host_end..],
        ended_with_colon,
    )
}

#[cfg(test)]
#[path = "register_args_tests.rs"]
mod tests;
