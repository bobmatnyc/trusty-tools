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

pub(crate) const RELEASES_API: &str =
    "https://api.github.com/repos/bobmatnyc/trusty-tools/releases";
pub(crate) const RELEASE_DL_BASE: &str =
    "https://github.com/bobmatnyc/trusty-tools/releases/download";

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

/// Resolve the release-asset filename prefix for a crate, handling
/// crate-name ⇄ asset-name aliases.
///
/// Why: A crate's Git tag prefix (used for release/tag resolution) does not
/// always match the filename prefix baked into its release-workflow tarball
/// name. `tga`'s package is named `tga` (and its tags are `tga-v*`), but its
/// release workflow names the asset after the crate's directory,
/// `trusty-git-analytics-<version>-<target>.tar.gz` — so a filename built
/// from the crate name alone 404s even though the correct tag resolved fine.
///
/// What: A small table of known aliases; falls through to `crate_name`
/// unchanged for every crate whose asset prefix matches its crate name (the
/// common case). Add a new entry here if another crate's release workflow
/// ever diverges the same way.
///
/// Test: `tests::asset_name_for_tag_resolves_tga_alias`,
/// `tests::asset_name_for_tag_defaults_to_crate_name`.
fn asset_name_for_tag(crate_name: &str) -> &str {
    match crate_name {
        "tga" => "trusty-git-analytics",
        other => other,
    }
}

/// Build the download URL for a prebuilt asset tarball.
///
/// Why: The URL pattern is `<base>/<tag>/<asset-name>-<version>-<target>.tar.gz`;
/// centralising construction avoids scatter and makes it trivially testable.
///
/// What: Returns the HTTPS URL for the `.tar.gz` asset. `crate_name` selects
/// the tag path (unaffected); the filename itself resolves through
/// [`asset_name_for_tag`] so aliased crates (e.g. `tga`) build the correct
/// asset filename.
///
/// Test: `tests::asset_url_shape`, `tests::asset_url_shape_tga_alias`.
pub fn asset_url(tag: &str, crate_name: &str, version: &str, target: &str) -> String {
    asset_url_at_base(RELEASE_DL_BASE, tag, crate_name, version, target)
}

/// [`asset_url`], against a caller-supplied download base.
///
/// Why: #5491's pinned path must be provable offline. Its tests point the whole
/// download→verify→extract pipeline at a loopback fixture server, which needs the
/// base to be injectable — the same seam [`resolve_latest_tag_from_url`] already
/// established for the releases API. Keeping ONE filename/URL construction and
/// parameterising the base (rather than a second `format!` in the pinned module)
/// means the `tga` asset-name alias cannot drift between the two paths.
///
/// What: Joins `base`, `tag`, and the alias-resolved asset filename.
///
/// Test: `tests::asset_url_at_base_honours_alias_and_base`; `asset_url`'s own
/// tests cover the production base by delegation.
pub(crate) fn asset_url_at_base(
    base: &str,
    tag: &str,
    crate_name: &str,
    version: &str,
    target: &str,
) -> String {
    let filename = asset_filename(asset_name_for_tag(crate_name), version, target);
    format!("{base}/{tag}/{filename}")
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

/// Why a pinned-version lookup failed (#5491).
///
/// Why: The pinned entry point must fail CLOSED, which means its caller needs to
/// distinguish "the release list was unreachable" from "that exact version was
/// never published" — the second carries the published set so the error can tell
/// the operator (or a GUI) what they could have pinned instead. A bare
/// `anyhow::Error` would flatten both into a string and force the caller to
/// re-parse it.
///
/// What: [`ResolveError::Fetch`] wraps any transport/deserialisation failure;
/// [`ResolveError::NotPublished`] carries every stable version published for the
/// crate. Crate-internal — the public surface is `download::pinned::PinnedError`.
///
/// Test: `tests::select_exact_version_rejects_absent_version` and the fail-closed
/// arms in `download::pinned::tests`.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ResolveError {
    /// The release list could not be fetched or parsed.
    #[error("could not fetch the release list: {0}")]
    Fetch(#[source] anyhow::Error),
    /// No stable release matched the pinned version.
    #[error("no stable release matched the pinned version")]
    NotPublished {
        /// Every stable version published for the crate, ascending.
        available: Vec<String>,
    },
}

/// Select the release matching an EXACT pinned version (#5491).
///
/// Why: [`select_highest_semver`] answers "what is newest", which is precisely
/// the wrong question for a pinned consumer — a caller asking for 2.9.4 must get
/// 2.9.4 or an error, never 2.9.5. This is the pure half of that guarantee, kept
/// separate from the HTTP call so the fail-closed decision is testable offline.
///
/// # Postconditions
/// On `Ok`, the returned tag's parsed version equals `version` exactly. On `Err`,
/// NOTHING resembling a fallback is returned — the caller cannot accidentally
/// proceed with a different version.
///
/// What: Matches `<crate_name>-v<version>` against the release list, skipping
/// prereleases (both the API flag and semver pre-release identifiers, matching
/// [`select_highest_semver`]'s conservative stance). On no match, returns the
/// published stable versions ascending so the error can list them.
///
/// Test: `tests::select_exact_version_picks_exact`,
/// `tests::select_exact_version_rejects_absent_version`,
/// `tests::select_exact_version_ignores_newer_releases`,
/// `tests::select_exact_version_skips_prerelease`.
fn select_exact_version(
    releases: &[GhRelease],
    crate_name: &str,
    version: &str,
) -> Result<ResolvedTag, ResolveError> {
    let prefix = format!("{crate_name}-v");
    // Normalise through semver so `2.9.4` and a tag written `2.9.4` compare by
    // value rather than by string — and so a caller-supplied non-semver pin is
    // rejected here rather than 404ing later against a URL built from garbage.
    let wanted = Version::parse(version).ok();
    let mut available: Vec<Version> = Vec::new();

    for release in releases {
        if release.prerelease {
            continue;
        }
        let Some(ver_str) = release.tag_name.strip_prefix(&prefix) else {
            continue;
        };
        let Ok(ver) = Version::parse(ver_str) else {
            continue;
        };
        if !ver.pre.is_empty() {
            continue;
        }
        if wanted.as_ref() == Some(&ver) {
            return Ok(ResolvedTag {
                tag: release.tag_name.clone(),
                version: ver.to_string(),
            });
        }
        available.push(ver);
    }

    available.sort();
    Err(ResolveError::NotPublished {
        available: available.iter().map(Version::to_string).collect(),
    })
}

/// Resolve the release tag for an EXACT pinned version of `crate_name` (#5491).
///
/// Why: The pinned install path never consults `latest`; this is the only
/// resolver it uses, so there is no code path along which a version drift can
/// enter.
///
/// What: Fetches the production releases API and delegates to
/// [`select_exact_version`].
///
/// Test: Live network — covered by the offline fixture tests on
/// [`resolve_pinned_tag_from_url`] and the pure `select_exact_version` tests.
pub async fn resolve_pinned_tag(
    client: &reqwest::Client,
    crate_name: &str,
    version: &str,
) -> anyhow::Result<ResolvedTag> {
    resolve_pinned_tag_from_url(client, RELEASES_API, crate_name, version)
        .await
        .map_err(anyhow::Error::new)
}

/// [`resolve_pinned_tag`], against a (possibly mock) releases URL.
///
/// Why: Same seam as [`resolve_latest_tag_from_url`] — it is what lets the
/// fail-closed arms run against a loopback fixture instead of real GitHub.
///
/// What: Fetches `url`, deserialises the release list, delegates to
/// [`select_exact_version`].
///
/// Test: `download::pinned::tests` drives this through the full entry point.
pub(crate) async fn resolve_pinned_tag_from_url(
    client: &reqwest::Client,
    url: &str,
    crate_name: &str,
    version: &str,
) -> Result<ResolvedTag, ResolveError> {
    let mut req = client.get(url).header("User-Agent", "trusty-installer");
    if let Some(token) = github_token() {
        req = req.header("Authorization", format!("Bearer {token}"));
    }
    let releases: Vec<GhRelease> = req
        .send()
        .await
        .with_context(|| format!("fetching GitHub releases from {url}"))
        .map_err(ResolveError::Fetch)?
        .error_for_status()
        .context("GitHub releases API returned an error status")
        .map_err(ResolveError::Fetch)?
        .json()
        .await
        .context("deserialising GitHub releases JSON")
        .map_err(ResolveError::Fetch)?;

    select_exact_version(&releases, crate_name, version)
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

    /// Why: `tga`'s release workflow names its asset after the crate
    /// directory (`trusty-git-analytics`), not the package name (`tga`); the
    /// alias table must resolve this so `try_install_prebuilt("tga", ..)`
    /// hits a real asset instead of 404ing.
    /// What: Asserts `asset_name_for_tag("tga") == "trusty-git-analytics"`.
    /// Test: This is the test.
    #[test]
    fn asset_name_for_tag_resolves_tga_alias() {
        assert_eq!(asset_name_for_tag("tga"), "trusty-git-analytics");
    }

    /// Why: Every crate WITHOUT a known alias must build its asset name from
    /// its own crate name unchanged — the common case must never regress.
    /// What: Asserts a handful of unaliased crate names pass through as-is.
    /// Test: This is the test.
    #[test]
    fn asset_name_for_tag_defaults_to_crate_name() {
        for c in ["trusty-search", "trusty-memory", "trusty-installer"] {
            assert_eq!(asset_name_for_tag(c), c);
        }
    }

    /// Why: The end-to-end URL for `tga` must use the ALIASED asset filename
    /// while keeping the crate's own tag in the URL path — this is the exact
    /// #<tga-asset-mismatch> bug: tag resolution and filename resolution use
    /// different names for this one crate.
    /// What: Builds a `tga` asset URL; asserts it contains
    /// `trusty-git-analytics-2.9.4-...` under the `tga-v2.9.4` tag path.
    /// Test: This is the test.
    #[test]
    fn asset_url_shape_tga_alias() {
        let url = asset_url("tga-v2.9.4", "tga", "2.9.4", "aarch64-apple-darwin");
        assert_eq!(
            url,
            "https://github.com/bobmatnyc/trusty-tools/releases/download/\
             tga-v2.9.4/trusty-git-analytics-2.9.4-aarch64-apple-darwin.tar.gz"
        );
    }

    /// Why: The base must be injectable for #5491's offline fixture tests, and
    /// the `tga` alias must survive that injection — if the pinned path built
    /// filenames independently, `tga` would 404 exactly as it did before the
    /// alias table existed.
    /// What: Builds a `tga` URL against a loopback base; asserts both the base
    /// and the aliased filename appear.
    /// Test: This is the test.
    #[test]
    fn asset_url_at_base_honours_alias_and_base() {
        let url = asset_url_at_base(
            "http://127.0.0.1:9/dl",
            "tga-v2.9.4",
            "tga",
            "2.9.4",
            "aarch64-apple-darwin",
        );
        assert_eq!(
            url,
            "http://127.0.0.1:9/dl/tga-v2.9.4/\
             trusty-git-analytics-2.9.4-aarch64-apple-darwin.tar.gz"
        );
    }

    /// Why: The pinned resolver must return the version the caller asked for.
    /// What: Asks for 0.24.1 from a list whose newest is 0.25.0; asserts the tag
    /// and version are the PINNED ones.
    /// Test: This is the test.
    #[test]
    fn select_exact_version_picks_exact() {
        let releases = vec![
            release("trusty-search-v0.25.0", false),
            release("trusty-search-v0.24.1", false),
        ];
        let rt = select_exact_version(&releases, "trusty-search", "0.24.1").unwrap();
        assert_eq!(rt.tag, "trusty-search-v0.24.1");
        assert_eq!(rt.version, "0.24.1");
    }

    /// Why: THE fail-closed guarantee at the resolver layer — a newer release
    /// existing must never satisfy a pin for a version that was never published.
    /// This is the exact drift `try_install_prebuilt`'s `latest` resolution
    /// would introduce.
    /// What: Pins 9.9.9 against a list containing only 0.25.0/0.24.1; asserts
    /// `NotPublished` carrying both published versions — not a silent 0.25.0.
    /// Test: This is the test.
    #[test]
    fn select_exact_version_rejects_absent_version() {
        let releases = vec![
            release("trusty-search-v0.25.0", false),
            release("trusty-search-v0.24.1", false),
        ];
        let err = select_exact_version(&releases, "trusty-search", "9.9.9")
            .expect_err("an unpublished pin must not resolve");
        match err {
            ResolveError::NotPublished { available } => {
                assert_eq!(available, vec!["0.24.1".to_owned(), "0.25.0".to_owned()]);
            }
            other => panic!("expected NotPublished, got {other:?}"),
        }
    }

    /// Why: Another crate's tag at the pinned version must not satisfy the pin.
    /// What: Pins trusty-search 0.18.0 when only trusty-memory-v0.18.0 exists.
    /// Test: This is the test.
    #[test]
    fn select_exact_version_ignores_other_crates() {
        let releases = vec![release("trusty-memory-v0.18.0", false)];
        assert!(select_exact_version(&releases, "trusty-search", "0.18.0").is_err());
    }

    /// Why: A prerelease must not satisfy a pin, matching the latest-path stance;
    /// otherwise pinning `0.26.0` could land an `0.26.0` release still flagged
    /// prerelease.
    /// What: Marks the matching entry `prerelease = true`; asserts Err.
    /// Test: This is the test.
    #[test]
    fn select_exact_version_skips_prerelease() {
        let releases = vec![release("trusty-search-v0.26.0", true)];
        assert!(select_exact_version(&releases, "trusty-search", "0.26.0").is_err());
    }

    /// Why: A non-semver pin must be rejected at the resolver rather than
    /// producing a URL built from garbage that 404s with a confusing message.
    /// What: Pins the literal string `latest`; asserts Err even though releases exist.
    /// Test: This is the test.
    #[test]
    fn select_exact_version_rejects_non_semver_pin() {
        let releases = vec![release("trusty-search-v0.25.0", false)];
        assert!(select_exact_version(&releases, "trusty-search", "latest").is_err());
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
