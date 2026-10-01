//! Where `tm content update` reads published content releases from (#8378 PR-C).
//!
//! Why: the update logic must be testable offline, with a release that is
//! missing, tampered with or unreachable, so the network sits behind a trait.
//! What: [`ReleaseSource`] — fetch one release asset, list the published
//! `content-v*` tags. [`GithubReleases`] is the production implementation over
//! GitHub's release-download URLs and its git-refs API.
//! Test: `github_source_reads_a_404_as_absent`,
//! `github_source_lists_content_tags`, `github_source_reports_an_unreachable_host`.

use std::io::Read;
use std::time::Duration;

use trusty_common::content::TAG_PREFIX;

/// The repository that publishes `content-v*` releases (ADR-0064 decision 4).
pub const CONTENT_REPO: &str = "bobmatnyc/trusty-tools";

/// Largest git-refs listing read.
const MAX_REFS_BYTES: u64 = 1024 * 1024;

/// A request that did not produce an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchError {
    /// The URL requested.
    pub url: String,
    /// Why it failed.
    pub reason: String,
}

/// A source of published content releases.
pub trait ReleaseSource {
    /// The URL [`ReleaseSource::asset`] reads for `file` of `tag`.
    fn asset_url(&self, tag: &str, file: &str) -> String;
    /// Fetches release asset `file` of `tag`, at most `max_bytes`. `Ok(None)`
    /// when the release or the asset does not exist.
    fn asset(&self, tag: &str, file: &str, max_bytes: u64) -> Result<Option<Vec<u8>>, FetchError>;
    /// Every published `content-v*` tag, in no particular order.
    fn content_tags(&self) -> Result<Vec<String>, FetchError>;
}

/// GitHub releases of [`CONTENT_REPO`].
///
/// Why: `content-release.yml` publishes each bundle and its sidecar as
/// release assets; releases are created with `--latest=false`, so GitHub's
/// "latest release" never names one, and the tag list is read from git refs.
/// What: assets from `<download_base>/<tag>/<file>`; tags from
/// `<api_base>/git/matching-refs/tags/content-v`. Bounded timeouts; every body
/// is read through a size cap.
/// Test: `github_source_reads_a_404_as_absent`, `github_source_lists_content_tags`,
/// `github_source_reads_a_non_404_error_status_as_a_failure`,
/// `github_source_refuses_a_refs_listing_that_is_not_json`.
#[derive(Debug, Clone)]
pub struct GithubReleases {
    client: reqwest::blocking::Client,
    download_base: String,
    api_base: String,
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
        })
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

    fn content_tags(&self) -> Result<Vec<String>, FetchError> {
        #[derive(serde::Deserialize)]
        struct GitRef {
            #[serde(rename = "ref")]
            name: String,
        }
        let url = format!("{}/git/matching-refs/tags/{TAG_PREFIX}", self.api_base);
        let body = self.get(&url, MAX_REFS_BYTES)?.unwrap_or_default();
        if body.is_empty() {
            return Ok(Vec::new());
        }
        let refs: Vec<GitRef> = serde_json::from_slice(&body).map_err(|e| FetchError {
            url: url.clone(),
            reason: format!("unexpected response: {e}"),
        })?;
        Ok(refs
            .into_iter()
            .filter_map(|r| r.name.strip_prefix("refs/tags/").map(str::to_owned))
            .collect())
    }
}
