//! GitHub Releases API queries and asset-URL construction for prebuilt binaries.
//!
//! Why: Each trusty-* crate tags its releases as `<crate>-v<semver>` (e.g.
//! `trusty-search-v0.25.0`). To pick the right prebuilt tarball we need to (a)
//! discover the highest semver for a given crate-name prefix, (b) compute the
//! download URL, and (c) honour `GITHUB_TOKEN`/`GH_TOKEN` to avoid rate-limiting
//! in CI.
//!
//! What: [`resolve_latest_tag`] calls the public Releases API, filters by crate
//! prefix, and returns the tag name + bare version for the highest semver.
//! [`asset_url`] / [`sha256_url`] build the download URLs deterministically from a
//! tag + version + target. Network calls carry a `reqwest::Client` so tests can
//! inject a mock URL without needing real GitHub.
//!
//! Test: `tests` cover URL construction and tag/semver selection with in-process
//! mock data. Real network calls are `#[ignore]`-tagged.

use anyhow::{anyhow, Context};
use semver::Version;
use serde::Deserialize;

const RELEASES_API: &str = "https://api.github.com/repos/bobmatnyc/trusty-tools/releases";
const RELEASE_DL_BASE: &str = "https://github.com/bobmatnyc/trusty-tools/releases/download";

/// A resolved release tag for a given crate.
///
/// Why: Callers need both the full tag (for URL construction) and the bare version
/// (for display / idempotency checks) — bundling them avoids reparsing.
///
/// What: `tag` is the full Git tag (e.g. `trusty-search-v0.25.0`); `version` is
/// the bare semver string (e.g. `0.25.0`).
///
/// Test: `tests::select_highest_semver` exercises the selection logic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedTag {
    /// Full Git tag, e.g. `trusty-search-v0.25.0`.
    pub tag: String,
    /// Bare version string, e.g. `0.25.0`.
    pub version: String,
}

/// Minimal shape of a GitHub Releases API entry (only the fields we need).
///
/// Why: Deserialise only what we consume; avoids brittle full-struct bindings.
/// What: `tag_name` is the Git tag string.
/// Test: Deserialisation is exercised by `tests::select_highest_semver`.
#[derive(Debug, Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    prerelease: bool,
}

/// Build the download URL for a prebuilt asset tarball.
///
/// Why: The URL pattern is `<base>/<tag>/<crate>-<version>-<target>.tar.gz`;
/// centralising construction avoids scatter and makes it trivially testable.
///
/// What: Returns the HTTPS URL for the `.tar.gz` asset.
///
/// Test: `tests::asset_url_shape`.
pub fn asset_url(tag: &str, crate_name: &str, version: &str, target: &str) -> String {
    let filename = asset_filename(crate_name, version, target);
    format!("{RELEASE_DL_BASE}/{tag}/{filename}")
}

/// Build the SHA-256 checksum URL for a prebuilt asset.
///
/// Why: The `.sha256` sidecar must be fetched and verified before trusting the
/// archive; centralising the URL avoids accidental divergence.
///
/// What: Returns `<asset_url>.sha256`.
///
/// Test: `tests::sha256_url_shape`.
pub fn sha256_url(tag: &str, crate_name: &str, version: &str, target: &str) -> String {
    format!("{}.sha256", asset_url(tag, crate_name, version, target))
}

/// The filename of the prebuilt asset tarball (without the download URL prefix).
///
/// Why: Both `asset_url` and `fetch` need to know the expected filename inside the
/// temp dir; extracting it avoids duplication.
///
/// What: Returns `<crate>-<version>-<target>.tar.gz`.
///
/// Test: `tests::asset_filename_shape`.
pub fn asset_filename(crate_name: &str, version: &str, target: &str) -> String {
    format!("{crate_name}-{version}-{target}.tar.gz")
}

/// Resolve the latest release tag for a given crate prefix from a releases JSON blob.
///
/// Why: The releases API returns releases in reverse-chronological order, but
/// semver ordering is the authoritative source of truth (a re-published older
/// release would confuse chronological selection). We parse every matching tag,
/// pick the highest semver, and skip pre-releases.
///
/// What: Accepts the raw deserialized `GhRelease` list (pre-fetched so the pure
/// selection logic is testable without HTTP). Returns the [`ResolvedTag`] with
/// the highest semver for `crate_name`, or an error if none found.
///
/// Test: `tests::select_highest_semver`, `tests::select_skips_prerelease`.
fn select_highest_semver(releases: &[GhRelease], crate_name: &str) -> anyhow::Result<ResolvedTag> {
    let prefix = format!("{crate_name}-v");
    let mut best: Option<(Version, ResolvedTag)> = None;

    for release in releases {
        if release.prerelease {
            continue;
        }
        let tag = &release.tag_name;
        let Some(ver_str) = tag.strip_prefix(&prefix) else {
            continue;
        };
        let Ok(ver) = Version::parse(ver_str) else {
            continue;
        };
        // Skip any semver with pre-release identifiers (conservative: stable only).
        if !ver.pre.is_empty() {
            continue;
        }
        let is_better = best.as_ref().is_none_or(|(best_ver, _)| &ver > best_ver);
        if is_better {
            best = Some((
                ver.clone(),
                ResolvedTag {
                    tag: tag.clone(),
                    version: ver.to_string(),
                },
            ));
        }
    }

    best.map(|(_, rt)| rt)
        .ok_or_else(|| anyhow!("no stable {crate_name}-v* release found"))
}

/// Optional GitHub auth token from the environment.
///
/// Why: The GitHub Releases API is rate-limited to 60 req/hr unauthenticated;
/// CI / power users can export `GITHUB_TOKEN` or `GH_TOKEN` to raise the limit.
///
/// What: Returns the value of `GITHUB_TOKEN` or `GH_TOKEN` (first set wins).
///
/// Test: The env-read is side-effecting; the token plumbing into the header is
/// tested via the live `#[ignore]` test.
fn github_token() -> Option<String> {
    std::env::var(trusty_common::env_vars::ENV_GITHUB_TOKEN)
        .or_else(|_| std::env::var("GH_TOKEN"))
        .ok()
        .filter(|s| !s.is_empty())
}

/// Resolve the latest stable release tag for `crate_name` using the GitHub API.
///
/// Why: `tctl install`/`upgrade` on a Tier-1 target need to know which tag to
/// download; the API is the authoritative source (no hardcoded BOM pin needed for
/// the prebuilt path).
///
/// What: Fetches `RELEASES_API`, deserialises the release list, delegates to
/// [`select_highest_semver`]. Optionally adds a `Authorization: Bearer` header
/// when a GitHub token is found in the environment.
///
/// Test: Live network call — `#[ignore]`-tagged. Pure selection is tested by
/// `tests::select_highest_semver`.
pub async fn resolve_latest_tag(
    client: &reqwest::Client,
    crate_name: &str,
) -> anyhow::Result<ResolvedTag> {
    resolve_latest_tag_from_url(client, RELEASES_API, crate_name).await
}

/// Resolve the latest stable tag from a (possibly mock) releases URL.
///
/// Why: Extracted from `resolve_latest_tag` so tests can supply a local mock URL
/// without mocking at the `reqwest` level.
///
/// What: Same logic as `resolve_latest_tag` but with a caller-supplied URL.
///
/// Test: `tests::resolve_latest_tag_live` (ignore-tagged live test).
pub(crate) async fn resolve_latest_tag_from_url(
    client: &reqwest::Client,
    url: &str,
    crate_name: &str,
) -> anyhow::Result<ResolvedTag> {
    let mut req = client.get(url).header("User-Agent", "trusty-installer");
    if let Some(token) = github_token() {
        req = req.header("Authorization", format!("Bearer {token}"));
    }
    let releases: Vec<GhRelease> = req
        .send()
        .await
        .with_context(|| format!("fetching GitHub releases from {url}"))?
        .error_for_status()
        .with_context(|| "GitHub releases API returned an error status")?
        .json()
        .await
        .with_context(|| "deserialising GitHub releases JSON")?;

    select_highest_semver(&releases, crate_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str, prerelease: bool) -> GhRelease {
        GhRelease {
            tag_name: tag.to_owned(),
            prerelease,
        }
    }

    /// Why: The highest semver must win regardless of list order; chronological
    /// order from the API must not fool the selection.
    /// What: Supplies releases in reverse-chronological (newest-first) order with
    /// an older entry that would win if chronological order were used; asserts the
    /// highest semver is selected.
    /// Test: This is the test.
    #[test]
    fn select_highest_semver_picks_max() {
        let releases = vec![
            release("trusty-search-v0.25.0", false),
            release("trusty-search-v0.24.1", false),
            release("trusty-memory-v0.18.0", false),
            release("trusty-search-v0.20.0", false),
        ];
        let rt = select_highest_semver(&releases, "trusty-search").unwrap();
        assert_eq!(rt.tag, "trusty-search-v0.25.0");
        assert_eq!(rt.version, "0.25.0");
    }

    /// Why: Pre-releases must not be selected — only stable releases are
    /// appropriate for the prebuilt-first install path.
    /// What: Marks the highest-versioned entry as `prerelease = true`; asserts
    /// the next stable version is selected.
    /// Test: This is the test.
    #[test]
    fn select_skips_prerelease() {
        let releases = vec![
            release("trusty-search-v0.26.0", true),
            release("trusty-search-v0.25.0", false),
        ];
        let rt = select_highest_semver(&releases, "trusty-search").unwrap();
        assert_eq!(rt.version, "0.25.0");
    }

    /// Why: Tags for OTHER crates in the same mono-repo must not match.
    /// What: Mixes trusty-memory and trusty-search tags; asserts only search
    /// tags are returned when querying "trusty-search".
    /// Test: This is the test.
    #[test]
    fn select_ignores_other_crates() {
        let releases = vec![
            release("trusty-memory-v1.0.0", false),
            release("trusty-search-v0.25.0", false),
        ];
        let rt = select_highest_semver(&releases, "trusty-search").unwrap();
        assert_eq!(rt.tag, "trusty-search-v0.25.0");
    }

    /// Why: An empty or prefix-mismatched release list must return an error, not panic.
    /// What: Calls `select_highest_semver` with no matching releases; asserts Err.
    /// Test: This is the test.
    #[test]
    fn select_returns_error_when_no_match() {
        let releases = vec![release("trusty-memory-v0.18.0", false)];
        assert!(select_highest_semver(&releases, "trusty-search").is_err());
    }

    /// Why: Tags with pre-release semver identifiers (e.g. `0.25.0-rc1`) must be
    /// excluded even when `prerelease = false` on the API entry.
    /// What: Supplies a tag `trusty-search-v0.26.0-rc1` with `prerelease = false`;
    /// asserts the stable `0.25.0` is selected.
    /// Test: This is the test.
    #[test]
    fn select_skips_semver_prerelease_identifiers() {
        let releases = vec![
            release("trusty-search-v0.26.0-rc1", false),
            release("trusty-search-v0.25.0", false),
        ];
        let rt = select_highest_semver(&releases, "trusty-search").unwrap();
        assert_eq!(rt.version, "0.25.0");
    }

    /// Why: The asset URL is a public contract; pin its shape to catch regressions.
    /// What: Builds an asset URL for trusty-search 0.25.0 on macOS arm64; asserts
    /// the full URL matches the expected pattern.
    /// Test: This is the test.
    #[test]
    fn asset_url_shape() {
        let url = asset_url(
            "trusty-search-v0.25.0",
            "trusty-search",
            "0.25.0",
            "aarch64-apple-darwin",
        );
        assert_eq!(
            url,
            "https://github.com/bobmatnyc/trusty-tools/releases/download/\
             trusty-search-v0.25.0/trusty-search-0.25.0-aarch64-apple-darwin.tar.gz"
        );
    }

    /// Why: The SHA-256 URL must be the asset URL with `.sha256` appended.
    /// What: Asserts `sha256_url` ends with `.tar.gz.sha256`.
    /// Test: This is the test.
    #[test]
    fn sha256_url_shape() {
        let url = sha256_url(
            "trusty-search-v0.25.0",
            "trusty-search",
            "0.25.0",
            "aarch64-apple-darwin",
        );
        assert!(url.ends_with(".tar.gz.sha256"), "got: {url}");
    }

    /// Why: The filename must follow the `<crate>-<version>-<target>.tar.gz` convention.
    /// What: Asserts the concatenation for a known set of inputs.
    /// Test: This is the test.
    #[test]
    fn asset_filename_shape() {
        let f = asset_filename("tga", "1.2.3", "x86_64-unknown-linux-gnu");
        assert_eq!(f, "tga-1.2.3-x86_64-unknown-linux-gnu.tar.gz");
    }

    /// Why: Live integration proof that the GitHub API is reachable and returns a
    /// trusty-search tag. Gated behind `#[ignore]` so CI stays offline-deterministic.
    /// What: Calls the real API; asserts a non-empty tag is returned.
    /// Test: `cargo test -p trusty-installer -- --include-ignored resolve_latest_tag_live`.
    #[tokio::test]
    #[ignore = "performs a live GitHub API call; run with --include-ignored"]
    async fn resolve_latest_tag_live() {
        let client = reqwest::Client::new();
        let rt = resolve_latest_tag(&client, "trusty-search")
            .await
            .expect("should resolve");
        assert!(!rt.tag.is_empty());
        assert!(!rt.version.is_empty());
        assert!(rt.tag.starts_with("trusty-search-v"));
    }
}
