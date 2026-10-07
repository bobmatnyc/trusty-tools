//! Doc paths named in a PR body or found by search (#9193).
//!
//! Why: the reviewer should read the ADR, spec or SLD a PR says it follows,
//! but the PR body is author text, so every path it names is untrusted.
//! What: [`extract_doc_paths`] reads `.md` paths from the body (bare, in
//! backticks or links, or as a `github.com/{owner}/{repo}/blob/<ref>/<path>`
//! URL of the reviewed repo), drops an SLD `#anchor`, validates each with
//! `validate_repo_path` and keeps those [`is_doc_path`] allows.
//! [`repo_relative`] turns a search hit into a repository path.
//! Test: `doc_refs_tests.rs`.

use crate::integrations::context::contents_at_ref::validate_repo_path;

/// Most distinct doc paths one body yields, so a hostile body costs O(1)
/// fetch planning.
pub(crate) const MAX_DOC_PATH_CANDIDATES: usize = 64;

/// Directories whose `.md` files are review docs.
const DOC_DIRS: [&str; 6] = [
    "docs/adr/",
    "docs/specs/",
    "docs/design/",
    "docs/prd/",
    "docs/architecture/",
    "docs/reference/",
];

/// Characters that end a path token in prose or markdown.
fn is_delimiter(c: char) -> bool {
    c.is_whitespace() || "`()[]<>\"'*,;|{}".contains(c)
}

/// Whether `path` is a review doc: a `.md` file under one of the doc
/// directories, or under `crates/<crate>/docs/`, and not a `CLAUDE.md`.
///
/// Test: `readme_and_node_modules_not_in_allowlist`, `search_hit_outside_allowlist_is_ignored`.
pub(crate) fn is_doc_path(path: &str) -> bool {
    let crate_docs = path
        .strip_prefix("crates/")
        .and_then(|rest| rest.split_once('/'))
        .is_some_and(|(name, rest)| !name.is_empty() && rest.starts_with("docs/"));
    path.ends_with(".md")
        && !is_claude_md(path)
        && (crate_docs || DOC_DIRS.iter().any(|d| path.starts_with(d)))
}

/// Whether `path` names a `CLAUDE.md` file.
pub(crate) fn is_claude_md(path: &str) -> bool {
    path.rsplit('/').next() == Some("CLAUDE.md")
}

/// The repository path a `github.com` blob URL names, when the URL is for
/// `owner/repo` (#9193 amendment 16: compared case-insensitively). The URL's
/// ref is dropped; docs are always read at the head SHA.
fn blob_path<'a>(rest: &'a str, owner: &str, repo: &str) -> Option<&'a str> {
    let mut parts = rest.splitn(5, '/');
    let (o, r, kind, _ref, path) = (
        parts.next()?,
        parts.next()?,
        parts.next()?,
        parts.next()?,
        parts.next()?,
    );
    let same = o.eq_ignore_ascii_case(owner) && r.eq_ignore_ascii_case(repo);
    (same && kind == "blob").then_some(path)
}

/// One token as a candidate path: URL prefix and anchor removed.
fn token_path<'a>(token: &'a str, owner: &str, repo: &str) -> Option<&'a str> {
    let token = token.trim_end_matches(['.', ':', '!', '?']);
    let lower = token.to_ascii_lowercase();
    let token = if let Some(at) = ["https://github.com/", "http://github.com/"]
        .iter()
        .find(|p| lower.starts_with(*p))
    {
        blob_path(&token[at.len()..], owner, repo)?
    } else {
        token
    };
    // SLD form `PATH#SPEC-…~rev`: the anchor is a hint; the whole file is read.
    let path = token.split('#').next().unwrap_or(token);
    path.ends_with(".md").then_some(path)
}

/// The review-doc paths `body` names, first-seen order, no repeats (#9193).
///
/// Why: plan §2 — the PR body is the primary source of doc paths.
/// What: splits `body` on prose and markdown delimiters; a token ending in
/// `.md` (after an SLD anchor and trailing punctuation are removed) is a
/// candidate, and a `github.com` URL counts only as a blob URL of
/// `owner/repo`. A candidate is kept when it passes `validate_repo_path` and
/// [`is_doc_path`] and is not a repeat; the scan stops at
/// [`MAX_DOC_PATH_CANDIDATES`] kept paths, so tokens outside the allowlist
/// never use up the budget (#9193 code-critic).
/// Test: `extracts_adr_spec_and_sld_paths`, `sld_anchor_is_split_off`,
/// `github_blob_url_ref_is_ignored`, `traversal_absolute_and_foreign_repo_paths_are_rejected`,
/// `foreign_repo_comparison_is_case_insensitive`, `a_thousand_paths_scan_is_bounded`,
/// `junk_md_tokens_never_crowd_out_a_doc_path`.
pub(crate) fn extract_doc_paths(body: &str, owner: &str, repo: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    // #9193: the cap counts kept doc paths, never raw `.md` tokens.
    let docs = body
        .split(is_delimiter)
        .filter(|t| t.contains(".md"))
        .filter_map(|t| token_path(t, owner, repo))
        .filter(|path| validate_repo_path(path).is_ok() && is_doc_path(path));
    for path in docs {
        if out.len() == MAX_DOC_PATH_CANDIDATES {
            break;
        }
        if !out.iter().any(|p| p == path) {
            out.push(path.to_string());
        }
    }
    out
}

/// A search hit's `file` as a repository path, or `None` (#9193 amendment 5).
///
/// Why: trusty-search may report an absolute path; only a path inside the
/// indexed root may be read, and only relative to it.
/// What: a relative path passes through; an absolute one has `root` and one
/// `/` stripped, and is `None` when `root` is unknown or not its prefix. The
/// result must pass `validate_repo_path`.
/// Test: `search_hit_absolute_path_is_made_repo_relative`.
pub(crate) fn repo_relative(file: &str, root: Option<&str>) -> Option<String> {
    let path = if file.starts_with('/') {
        let root = root?.trim_end_matches('/');
        file.strip_prefix(root)?.strip_prefix('/')?
    } else {
        file.strip_prefix("./").unwrap_or(file)
    };
    validate_repo_path(path).ok().map(|()| path.to_string())
}

#[cfg(test)]
#[path = "doc_refs_tests.rs"]
mod tests;
