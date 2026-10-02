//! Where `tm content update` reads published content releases from (#8378 PR-C).
//!
//! Why: the update logic must be testable offline, with a release that is
//! missing, tampered with or unreachable, so the network sits behind a trait.
//! What: [`ReleaseSource`] — fetch one release asset, list the `content-v*`
//! tags, read one release. [`GithubReleases`] is the production
//! implementation over GitHub's release-download URLs, its
//! `git/matching-refs` endpoint and `releases/tags/{tag}`.
//! Test: `github_source_reads_a_404_as_absent`,
//! `github_source_lists_every_content_tag_in_one_request`,
//! `github_source_reports_an_unreachable_host`.

use std::io::Read;
use std::time::Duration;

use trusty_common::content::TAG_PREFIX;

/// The repository that publishes `content-v*` releases (ADR-0064 decision 4).
pub const CONTENT_REPO: &str = "bobmatnyc/trusty-tools";

/// Largest API response read; one ref is about 400 bytes, so this holds
/// about 10 000 `content-v*` tags. A longer listing is refused, not truncated.
pub(crate) const MAX_LISTING_BYTES: u64 = 4 * 1024 * 1024;

/// A request that did not produce an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchError {
    /// The URL requested.
    pub url: String,
    /// Why it failed.
    pub reason: String,
}

/// One `content-v*` release as `releases/tags/{tag}` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// The release's tag.
    pub tag: String,
    /// An unpublished draft.
    pub draft: bool,
    /// Marked as a pre-release.
    pub prerelease: bool,
}

/// A source of published content releases.
pub trait ReleaseSource {
    /// The URL [`ReleaseSource::asset`] reads for `file` of `tag`.
    fn asset_url(&self, tag: &str, file: &str) -> String;
    /// Fetches release asset `file` of `tag`, at most `max_bytes`. `Ok(None)`
    /// when the release or the asset does not exist.
    fn asset(&self, tag: &str, file: &str, max_bytes: u64) -> Result<Option<Vec<u8>>, FetchError>;
    /// Every git tag starting `content-v`, in no particular order. A listing
    /// that cannot be read in full is an error; an empty one is `Ok`.
    fn content_tags(&self) -> Result<Vec<String>, FetchError>;
    /// The release of `tag`. A tag with no release, or whose release is a
    /// draft (GitHub answers 404 for one), is an error.
    fn release(&self, tag: &str) -> Result<Release, FetchError>;
}

/// GitHub releases of [`CONTENT_REPO`].
///
/// Why: `content-release.yml` publishes each bundle and its sidecar as
/// release assets. The whole `/releases` listing is megabytes against a
/// 60 requests/hour unauthenticated limit, so the newest tag is found by
/// listing only `content-v*` refs, and only that tag's release is read (#9036).
/// A tag alone is not a release (it may be a draft or pre-release), so the
/// chosen tag's release is still read before anything is installed (#8389).
/// What: assets from `<download_base>/<tag>/<file>`; tags from one unpaged
/// read of `<api_base>/git/matching-refs/tags/content-v`, which GitHub answers
/// in full (it ignores `per_page`/`page`), bounded by `MAX_LISTING_BYTES`;
/// one release from `<api_base>/releases/tags/<tag>`, which answers 404 for a
/// draft.
/// API calls send `Authorization: Bearer <token>` when a token is set; the
/// token is marked sensitive and redacted from `Debug`. Bounded timeouts;
/// every body is read through a size cap.
/// Test: `github_source_reads_a_404_as_absent`,
/// `github_source_lists_every_content_tag_in_one_request`,
/// `github_source_reads_a_non_404_error_status_as_a_failure`,
/// `github_source_refuses_a_tag_listing_that_is_not_json`,
/// `github_source_refuses_a_tag_listing_over_its_size_cap`,
/// `github_source_refuses_a_malformed_or_mismatched_answer`,
/// `github_source_reads_a_404_tag_listing_as_a_failure`,
/// `github_source_reads_a_rate_limited_or_5xx_response_as_a_failure`,
/// `update_finds_the_newest_release_in_two_api_requests`,
/// `github_source_sends_the_token_only_when_one_is_set`,
/// `a_token_never_appears_in_debug_or_error_output`.
#[derive(Clone)]
pub struct GithubReleases {
    client: reqwest::blocking::Client,
    download_base: String,
    api_base: String,
    token: Option<String>,
}

impl std::fmt::Debug for GithubReleases {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GithubReleases")
            .field("download_base", &self.download_base)
            .field("api_base", &self.api_base)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .finish_non_exhaustive()
    }
}

/// The GitHub token from the environment: `GITHUB_TOKEN`, else `GH_TOKEN`
/// (the order `trusty-installer` reads).
fn github_token_from_env() -> Option<String> {
    github_token_from(|name| std::env::var(name).ok())
}

/// A token trimmed of surrounding whitespace; empty or whitespace-only is unset.
fn normalize_token(token: String) -> Option<String> {
    let trimmed = token.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// The first set (non-blank, trimmed) of `GITHUB_TOKEN` and `GH_TOKEN` as
/// `var` reads them.
/// #9036: an empty or whitespace-only `GITHUB_TOKEN` falls through to
/// `GH_TOKEN`; a blank `GH_TOKEN` is unset.
/// Test: `an_empty_github_token_falls_through_to_gh_token`.
pub(super) fn github_token_from(var: impl Fn(&str) -> Option<String>) -> Option<String> {
    [trusty_common::env_vars::ENV_GITHUB_TOKEN, "GH_TOKEN"]
        .into_iter()
        .filter_map(var)
        .find_map(normalize_token)
}

impl GithubReleases {
    /// The production source; authenticates when `GITHUB_TOKEN`/`GH_TOKEN` is set.
    pub fn new() -> Result<Self, FetchError> {
        Ok(Self::with_bases(
            &format!("https://github.com/{CONTENT_REPO}/releases/download"),
            &format!("https://api.github.com/repos/{CONTENT_REPO}"),
        )?
        .with_token(github_token_from_env()))
    }

    /// A source reading from other base URLs, unauthenticated; tests point it
    /// at a local server.
    pub fn with_bases(download_base: &str, api_base: &str) -> Result<Self, FetchError> {
        let client = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(120))
            .user_agent(concat!("trusty-mpm/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| FetchError {
                url: download_base.to_owned(),
                reason: e.to_string(),
            })?;
        Ok(Self {
            client,
            download_base: download_base.trim_end_matches('/').to_owned(),
            api_base: api_base.trim_end_matches('/').to_owned(),
            token: None,
        })
    }

    /// The same source sending `token` on API calls; `None`, an empty or a
    /// whitespace-only token stays unauthenticated (#9036).
    pub fn with_token(mut self, token: Option<String>) -> Self {
        self.token = token.and_then(normalize_token);
        self
    }

    /// `auth` adds the bearer token (API calls only, never asset downloads).
    fn get(&self, url: &str, max_bytes: u64, auth: bool) -> Result<Option<Vec<u8>>, FetchError> {
        let fail = |reason: String| FetchError {
            url: url.to_owned(),
            reason,
        };
        let mut request = self.client.get(url);
        if let (true, Some(token)) = (auth, &self.token) {
            // #9036: the reason never carries the token or the header value.
            let mut value = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
                .map_err(|_| fail("the GitHub token is not a valid header value".to_owned()))?;
            value.set_sensitive(true);
            request = request.header(reqwest::header::AUTHORIZATION, value);
        }
        let response = request.send().map_err(|e| fail(e.to_string()))?;
        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !status.is_success() {
            return Err(fail(format!("HTTP {status}")));
        }
        let mut body = Vec::new();
        response
            .take(max_bytes.saturating_add(1))
            .read_to_end(&mut body)
            .map_err(|e| fail(e.to_string()))?;
        if body.len() as u64 > max_bytes {
            return Err(fail(format!("the response is over {max_bytes} bytes")));
        }
        Ok(Some(body))
    }

    /// An API read; the repository exists, so a 404 is a failure, not "none".
    fn api(&self, url: &str) -> Result<Vec<u8>, FetchError> {
        self.get(url, MAX_LISTING_BYTES, true)?
            .ok_or_else(|| FetchError {
                url: url.to_owned(),
                reason: "HTTP 404 Not Found".to_owned(),
            })
    }
}

impl ReleaseSource for GithubReleases {
    fn asset_url(&self, tag: &str, file: &str) -> String {
        format!("{}/{tag}/{file}", self.download_base)
    }

    fn asset(&self, tag: &str, file: &str, max_bytes: u64) -> Result<Option<Vec<u8>>, FetchError> {
        self.get(&self.asset_url(tag, file), max_bytes, false)
    }

    fn content_tags(&self) -> Result<Vec<String>, FetchError> {
        #[derive(serde::Deserialize)]
        struct ApiRef {
            r#ref: String,
        }
        // #9036: matching-refs ignores `per_page` and `page` and sends no Link
        // header, so one unpaged read is the whole list; paging re-reads it.
        let url = format!("{}/git/matching-refs/tags/{TAG_PREFIX}", self.api_base);
        let refs: Vec<ApiRef> =
            serde_json::from_slice(&self.api(&url)?).map_err(|e| FetchError {
                url: url.clone(),
                reason: format!("unexpected response: {e}"),
            })?;
        refs.into_iter()
            .map(|entry| {
                // A ref outside refs/tags/ is not what was asked for: refuse it.
                entry
                    .r#ref
                    .strip_prefix("refs/tags/")
                    .map(str::to_owned)
                    .ok_or_else(|| FetchError {
                        url: url.clone(),
                        reason: format!("unexpected ref {:?}", entry.r#ref),
                    })
            })
            .collect()
    }

    fn release(&self, tag: &str) -> Result<Release, FetchError> {
        #[derive(serde::Deserialize)]
        struct ApiRelease {
            tag_name: String,
            draft: bool,
            prerelease: bool,
        }
        let url = format!("{}/releases/tags/{tag}", self.api_base);
        let release: ApiRelease =
            serde_json::from_slice(&self.api(&url)?).map_err(|e| FetchError {
                url: url.clone(),
                reason: format!("unexpected response: {e}"),
            })?;
        if release.tag_name != tag {
            return Err(FetchError {
                url,
                reason: format!("asked for release {tag}, got {}", release.tag_name),
            });
        }
        Ok(Release {
            tag: release.tag_name,
            draft: release.draft,
            prerelease: release.prerelease,
        })
    }
}
