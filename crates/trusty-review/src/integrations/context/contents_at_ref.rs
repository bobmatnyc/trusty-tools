//! Repository files read through the Contents API at one commit (#9193).
//!
//! Why: an ADR or spec a PR names must be read as the PR's head commit holds
//! it, not as the default branch does; `GithubContentsFetch` reads the
//! default branch, validates no path, and cannot tell a directory or a
//! too-large file from a parse error.
//! What: [`DocFetcher`] is the seam (`Ok(None)` is a 404 for the path).
//! [`validate_repo_path`] and [`is_head_sha`] are checked before any URL is
//! built; [`contents_url`] builds the request URL with `ref=<sha>`;
//! [`classify_response`] maps a status and body to a result; and
//! [`GithubDocFetcher`] is the production client.
//! Test: `contents_at_ref_tests.rs`.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;

use super::external_spec::decode_base64_content;

/// Longest repository path a doc reference may name.
pub(crate) const MAX_DOC_PATH_CHARS: usize = 200;

/// Why a doc could not be read (#9193). Every variant omits the doc.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum DocFetchError {
    /// The path failed validation; no request was made.
    #[error("path rejected: {0}")]
    InvalidPath(String),
    /// The ref is not a full lowercase hex commit SHA; no request was made.
    #[error("ref is not a full commit SHA")]
    InvalidSha,
    /// The commit is not readable from this repository (a fork head, #9193).
    #[error("the head commit is not readable from this repository: {0}")]
    MissingCommit(String),
    /// The path names a directory.
    #[error("path is a directory, not a file")]
    Directory,
    /// The API returned no inline text (a file over 1 MB, a symlink, a submodule).
    #[error("no inline content: {0}")]
    NoInlineContent(String),
    /// The content did not decode to UTF-8 text.
    #[error("content is not UTF-8 text: {0}")]
    Undecodable(String),
    /// Any other non-2xx response.
    #[error("GitHub API returned {status}: {body}")]
    Api { status: u16, body: String },
    /// Transport failure or an unreadable response.
    #[error("GitHub request failed: {0}")]
    Transport(String),
}

/// Reads one repository file at one commit (#9193).
///
/// Why: the review reads docs through this seam so tests never dial GitHub.
/// What: `Ok(Some(text))` for a file, `Ok(None)` when the path does not exist
/// at `sha`, `Err` for every other failure.
/// Test: `fake_fetcher_serves_different_text_per_sha`.
#[async_trait]
pub(crate) trait DocFetcher: Send + Sync {
    /// The text of `path` at commit `sha`.
    async fn fetch(&self, path: &str, sha: &str) -> Result<Option<String>, DocFetchError>;
}

/// Whether `sha` is a full lowercase hex commit SHA (40, or 64 for SHA-256).
///
/// Test: `malformed_sha_never_fetches`, `head_sha_shape_is_strict`.
pub(crate) fn is_head_sha(sha: &str) -> bool {
    matches!(sha.len(), 40 | 64) && sha.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Check a repository-relative path before it reaches a URL (#9193).
///
/// Why: the path comes from PR-author text or a search hit, so it is
/// untrusted; a `..` segment, an encoded byte or a query character must never
/// reach the request.
/// What: non-empty, at most [`MAX_DOC_PATH_CHARS`] characters, only
/// `[A-Za-z0-9._/-]`, no leading `/`, and no empty, `.` or `..` segment.
///
/// # Errors
///
/// [`DocFetchError::InvalidPath`] naming the rule the path broke.
///
/// Test: `traversal_absolute_and_foreign_repo_paths_are_rejected`,
/// `fetcher_rejects_a_bad_path_without_a_request`.
pub(crate) fn validate_repo_path(path: &str) -> Result<(), DocFetchError> {
    let reject = |why: &str| Err(DocFetchError::InvalidPath(format!("{why}: {path:?}")));
    if path.is_empty() || path.chars().count() > MAX_DOC_PATH_CHARS {
        return reject("empty or longer than 200 characters");
    }
    let allowed = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | '-');
    if !path.chars().all(allowed) {
        return reject("character outside [A-Za-z0-9._/-]");
    }
    if path
        .split('/')
        .any(|s| s.is_empty() || s == "." || s == "..")
    {
        return reject("empty, '.' or '..' segment");
    }
    Ok(())
}

/// The Contents API URL for `path` at `sha` in `owner/repo` (#9193).
///
/// Why: the one place the `ref` is attached, so a test can prove every
/// request names the head commit.
/// What: validates owner, repo, path and SHA, then builds
/// `https://api.github.com/repos/{owner}/{repo}/contents/{path}?ref={sha}`.
///
/// # Errors
///
/// [`DocFetchError::InvalidPath`] or [`DocFetchError::InvalidSha`]; no URL is
/// built.
///
/// Test: `contents_url_carries_ref_equal_to_head_sha`.
pub(crate) fn contents_url(
    owner: &str,
    repo: &str,
    path: &str,
    sha: &str,
) -> Result<reqwest::Url, DocFetchError> {
    let name_ok = |s: &str| {
        !s.is_empty()
            && !s.starts_with('.')
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    };
    if !name_ok(owner) || !name_ok(repo) {
        return Err(DocFetchError::InvalidPath(format!(
            "repository {owner:?}/{repo:?}"
        )));
    }
    validate_repo_path(path)?;
    if !is_head_sha(sha) {
        return Err(DocFetchError::InvalidSha);
    }
    let raw = format!("https://api.github.com/repos/{owner}/{repo}/contents/{path}");
    let mut url = reqwest::Url::parse(&raw)
        .map_err(|e| DocFetchError::InvalidPath(format!("{path:?}: {e}")))?;
    url.query_pairs_mut().append_pair("ref", sha);
    Ok(url)
}

/// Map one Contents API response to a fetch result (#9193).
///
/// Why: the decision a directory, a fork head or a large file reaches must be
/// testable without a network.
/// What: 200 with a `file` object whose `encoding` is `base64` decodes to
/// text; a 200 array is a directory; any other 200 shape has no inline
/// content. A 404 or 422 whose body says no commit was found is
/// [`DocFetchError::MissingCommit`]; any other 404 is `Ok(None)`. Every other
/// status is [`DocFetchError::Api`] with the body cut to 300 characters.
///
/// # Errors
///
/// Every outcome but a decoded file and a plain 404.
///
/// Test: `classify_maps_each_response_shape`,
/// `fork_head_sha_unresolvable_is_unavailable_not_absent`.
pub(crate) fn classify_response(status: u16, body: &str) -> Result<Option<String>, DocFetchError> {
    let short: String = body.chars().take(300).collect();
    if matches!(status, 404 | 422) && body.to_ascii_lowercase().contains("no commit found") {
        return Err(DocFetchError::MissingCommit(short));
    }
    match status {
        404 => return Ok(None),
        200 => {}
        _ => {
            return Err(DocFetchError::Api {
                status,
                body: short,
            });
        }
    }
    let value: Value =
        serde_json::from_str(body).map_err(|e| DocFetchError::Transport(format!("JSON: {e}")))?;
    if value.is_array() {
        return Err(DocFetchError::Directory);
    }
    let field = |k: &str| value.get(k).and_then(Value::as_str).unwrap_or("");
    if field("type") != "file" || field("encoding") != "base64" {
        return Err(DocFetchError::NoInlineContent(format!(
            "type {:?}, encoding {:?}",
            field("type"),
            field("encoding")
        )));
    }
    decode_base64_content(field("content"))
        .map(Some)
        .map_err(DocFetchError::Undecodable)
}

/// The production [`DocFetcher`]: `GET /repos/{owner}/{repo}/contents/{path}?ref={sha}`.
///
/// Why: the head SHA is the only ref a doc is read at (#9193 criterion 1).
/// What: holds `owner/repo`, a resolved bearer token and a client with a
/// 10-second timeout; each fetch builds its URL with [`contents_url`] and
/// maps the reply with [`classify_response`]. The token is never logged.
/// Test: `contents_url_carries_ref_equal_to_head_sha`, `classify_maps_each_response_shape`.
pub(crate) struct GithubDocFetcher {
    owner: String,
    repo: String,
    token: String,
    client: reqwest::Client,
}

impl GithubDocFetcher {
    /// A fetcher for `owner/repo` with a resolved `token`.
    ///
    /// # Errors
    ///
    /// [`DocFetchError::Transport`] when the HTTP client cannot be built.
    pub(crate) fn new(owner: &str, repo: &str, token: &str) -> Result<Self, DocFetchError> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| DocFetchError::Transport(format!("HTTP client: {e}")))?;
        Ok(Self {
            owner: owner.to_string(),
            repo: repo.to_string(),
            token: token.to_string(),
            client,
        })
    }
}

#[async_trait]
impl DocFetcher for GithubDocFetcher {
    async fn fetch(&self, path: &str, sha: &str) -> Result<Option<String>, DocFetchError> {
        let url = contents_url(&self.owner, &self.repo, path, sha)?;
        let response = self
            .client
            .get(url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", "trusty-review")
            .send()
            .await
            .map_err(|e| DocFetchError::Transport(e.without_url().to_string()))?;
        let status = response.status().as_u16();
        let body = response
            .text()
            .await
            .map_err(|e| DocFetchError::Transport(e.without_url().to_string()))?;
        classify_response(status, &body)
    }
}

#[cfg(test)]
#[path = "contents_at_ref_tests.rs"]
mod tests;
