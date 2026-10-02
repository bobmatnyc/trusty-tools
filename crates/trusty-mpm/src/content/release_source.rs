//! Where `tm content update` reads published content releases from (#8378 PR-C).
//!
//! Why: the update logic must be testable offline, with a release that is
//! missing, tampered with or unreachable, so the network sits behind a trait.
//! What: [`ReleaseSource`] — fetch one release asset, list the `content-v*`
//! releases. [`GithubReleases`] is the production implementation over
//! GitHub's release-download URLs and its releases API.
//! Test: `github_source_reads_a_404_as_absent`,
//! `github_source_lists_content_releases_across_pages`,
//! `github_source_reports_an_unreachable_host`.

use std::io::Read;
use std::time::Duration;

use trusty_common::content::TAG_PREFIX;

/// The repository that publishes `content-v*` releases (ADR-0064 decision 4).
pub const CONTENT_REPO: &str = "bobmatnyc/trusty-tools";

/// Releases requested per page of the releases API (its maximum).
const RELEASES_PER_PAGE: usize = 100;

/// Pages read before the listing is refused as too long to read to the end.
pub const MAX_RELEASE_PAGES: u32 = 50;

/// Largest page of the releases API read; a real page is about 1.6 MB.
const MAX_LISTING_BYTES: u64 = 16 * 1024 * 1024;

/// A request that did not produce an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchError {
    /// The URL requested.
    pub url: String,
    /// Why it failed.
    pub reason: String,
}

/// One `content-v*` release as the releases API lists it.
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
    /// Every `content-v*` release, drafts and pre-releases included, in no
    /// particular order. A listing that cannot be read in full is an error.
    fn content_releases(&self) -> Result<Vec<Release>, FetchError>;
}

/// GitHub releases of [`CONTENT_REPO`].
///
/// Why: `content-release.yml` publishes each bundle and its sidecar as
/// release assets. A `content-v*` git tag alone is not a release: it may have
/// no release, or a draft or pre-release one, so the listing comes from the
/// releases API, not from git refs (#8389).
/// What: assets from `<download_base>/<tag>/<file>`; releases from
/// `<api_base>/releases`, paged to the end (at most `max_pages` pages). Bounded
/// timeouts; every body is read through a size cap.
/// Test: `github_source_reads_a_404_as_absent`,
/// `github_source_lists_content_releases_across_pages`,
/// `github_source_reads_a_non_404_error_status_as_a_failure`,
/// `github_source_refuses_a_release_listing_that_is_not_json`,
/// `github_source_refuses_a_listing_longer_than_its_page_cap`,
/// `github_source_reads_a_404_release_listing_as_a_failure`,
/// `github_source_reads_a_rate_limited_or_5xx_listing_as_a_failure`.
#[derive(Debug, Clone)]
pub struct GithubReleases {
    client: reqwest::blocking::Client,
    download_base: String,
    api_base: String,
    max_pages: u32,
}

impl GithubReleases {
    /// The production source.
    pub fn new() -> Result<Self, FetchError> {
        Self::with_bases(
            &format!("https://github.com/{CONTENT_REPO}/releases/download"),
            &format!("https://api.github.com/repos/{CONTENT_REPO}"),
        )
    }

    /// A source reading from other base URLs; tests point it at a local server.
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
            max_pages: MAX_RELEASE_PAGES,
        })
    }

    /// The same source with another page cap.
    pub fn with_max_pages(mut self, max_pages: u32) -> Self {
        self.max_pages = max_pages;
        self
    }

    fn get(&self, url: &str, max_bytes: u64) -> Result<Option<Vec<u8>>, FetchError> {
        let fail = |reason: String| FetchError {
            url: url.to_owned(),
            reason,
        };
        let response = self
            .client
            .get(url)
            .send()
            .map_err(|e| fail(e.to_string()))?;
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
}

impl ReleaseSource for GithubReleases {
    fn asset_url(&self, tag: &str, file: &str) -> String {
        format!("{}/{tag}/{file}", self.download_base)
    }

    fn asset(&self, tag: &str, file: &str, max_bytes: u64) -> Result<Option<Vec<u8>>, FetchError> {
        self.get(&self.asset_url(tag, file), max_bytes)
    }

    fn content_releases(&self) -> Result<Vec<Release>, FetchError> {
        #[derive(serde::Deserialize)]
        struct ApiRelease {
            tag_name: String,
            draft: bool,
            prerelease: bool,
        }
        let mut found = Vec::new();
        for page in 1..=self.max_pages {
            let url = format!(
                "{}/releases?per_page={RELEASES_PER_PAGE}&page={page}",
                self.api_base
            );
            // The repository always exists, so a 404 is a failure, not "none".
            let body = self
                .get(&url, MAX_LISTING_BYTES)?
                .ok_or_else(|| FetchError {
                    url: url.clone(),
                    reason: "HTTP 404 Not Found".to_owned(),
                })?;
            let releases: Vec<ApiRelease> =
                serde_json::from_slice(&body).map_err(|e| FetchError {
                    url: url.clone(),
                    reason: format!("unexpected response: {e}"),
                })?;
            let last_page = releases.len() < RELEASES_PER_PAGE;
            found.extend(
                releases
                    .into_iter()
                    .filter(|r| r.tag_name.starts_with(TAG_PREFIX))
                    .map(|r| Release {
                        tag: r.tag_name,
                        draft: r.draft,
                        prerelease: r.prerelease,
                    }),
            );
            if last_page {
                return Ok(found);
            }
        }
        Err(FetchError {
            url: format!("{}/releases", self.api_base),
            reason: format!(
                "more than {} pages of releases; the listing was not read to the end",
                self.max_pages
            ),
        })
    }
}
