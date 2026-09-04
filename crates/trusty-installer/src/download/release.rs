//! GitHub Releases API queries and asset-URL construction for prebuilt binaries.
//!
//! Why: Each trusty-* crate tags its releases as `<crate>-v<semver>` (e.g.
//! `trusty-search-v0.25.0`). To pick the right prebuilt tarball we need to (a)
//! discover the highest semver for a given crate-name prefix, (b) compute the
//! download URL, and (c) honour `GITHUB_TOKEN`/`GH_TOKEN` to avoid rate-limiting
//! in CI.
//!
//! What: [`resolve_latest_tag`] calls the public Releases API, filters by crate
//! prefix, and returns the tag name + bare version for the highest semver. A
//! crate whose tag spelling differs from its package name (`tga`, tagged
//! `trusty-git-analytics-v*` by the publish gate) matches under EITHER spelling
//! — see [`tag_name_candidates`] and #6771.
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
    /// The commit (or branch) the release points at. `None` on older payloads.
    #[serde(default)]
    target_commitish: Option<String>,
    /// Published assets, used to compare two spellings of the same release.
    #[serde(default)]
    assets: Vec<GhAsset>,
}

/// One published release asset (only the fields the split check reads).
///
/// Why: #6771 needs to tell "the same release under two tag spellings" from
/// "two different releases that happen to share a version". Identical asset
/// digests are the strongest available evidence of the first.
/// What: `name` is the filename; `digest` is GitHub's `sha256:<hex>` string,
/// absent on older payloads and on assets uploaded before digests existed.
/// Test: `tests::pinned_tag_resolves_when_both_spellings_agree`,
/// `tests::pinned_tag_errors_when_spellings_carry_different_digests`.
#[derive(Debug, Deserialize)]
struct GhAsset {
    name: String,
    #[serde(default)]
    digest: Option<String>,
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

/// Every `<name>-v*` tag spelling that may carry this crate's releases (#6771).
///
/// Why: the release tag and the crate name disagree for `tga`.
/// `scripts/check-publish-ready.sh` derives the tag from the crate DIRECTORY
/// (`trusty-git-analytics-v*`) while a `tga` pin looked only for `tga-v*`, so a
/// normally-published tga release was invisible to the installer and
/// `taudit install` reported it as unpublished. Resolution now accepts either
/// spelling instead of requiring a hand-pushed alias tag.
///
/// What: returns the candidate names in preference order — the caller's own
/// spelling first, then aliases. Every crate without an alias yields exactly
/// its own name, so the common path is unchanged.
///
/// Test: `tests::tag_name_candidates_covers_both_tga_spellings`,
/// `tests::tag_name_candidates_defaults_to_crate_name`.
fn tag_name_candidates(crate_name: &str) -> Vec<&str> {
    match crate_name {
        "tga" => vec!["tga", "trusty-git-analytics"],
        "trusty-git-analytics" => vec!["trusty-git-analytics", "tga"],
        other => vec![other],
    }
}

/// Every stable release matching any tag spelling for `crate_name` (#6771).
///
/// Why: both selectors need the same "which releases are this crate's" answer,
/// including the alias spellings, and both must skip prereleases identically.
///
/// What: returns `(candidate index, version, release)` for each stable match,
/// where the candidate index is the position in [`tag_name_candidates`] and so
/// orders preference when one version exists under two spellings. Prereleases
/// are skipped on both the API flag and semver pre-release identifiers.
///
/// Test: `tests::pinned_tag_resolves_from_directory_name_tag`,
/// `tests::latest_tag_spans_both_spellings`.
fn stable_matches<'r>(
    releases: &'r [GhRelease],
    crate_name: &str,
) -> Vec<(usize, Version, &'r GhRelease)> {
    let candidates = tag_name_candidates(crate_name);
    let mut out = Vec::new();
    for release in releases {
        if release.prerelease {
            continue;
        }
        for (rank, candidate) in candidates.iter().enumerate() {
            let prefix = format!("{candidate}-v");
            let Some(ver_str) = release.tag_name.strip_prefix(&prefix) else {
                continue;
            };
            let Ok(ver) = Version::parse(ver_str) else {
                break;
            };
            if ver.pre.is_empty() {
                out.push((rank, ver, release));
            }
            break;
        }
    }
    out
}

/// Whether two releases at the same version are the SAME release (#6771).
///
/// Why: accepting either tag spelling must not silently pick one when the two
/// tags name genuinely different builds — that would install an artifact the
/// operator never pinned. This decides whether a pick is safe.
///
/// What: two full 40-hex commit SHAs that differ are a split. A release cut
/// from an existing tag can carry a BRANCH name in `target_commitish`, so any
/// other pair of values is not comparable evidence and is ignored. An asset
/// published under both tags with differing digests is a split regardless.
///
/// Test: `tests::pinned_tag_resolves_when_both_spellings_agree`,
/// `tests::pinned_tag_errors_when_spellings_carry_different_digests`.
fn releases_agree(a: &GhRelease, b: &GhRelease) -> bool {
    let is_sha = |s: &String| s.len() == 40 && s.chars().all(|c| c.is_ascii_hexdigit());
    if let (Some(x), Some(y)) = (&a.target_commitish, &b.target_commitish) {
        if is_sha(x) && is_sha(y) && x != y {
            return false;
        }
    }
    !a.assets.iter().any(|asset| {
        let Some(left) = asset.digest.as_ref() else {
            return false;
        };
        b.assets.iter().any(|other| {
            other.name == asset.name && other.digest.as_ref().is_some_and(|r| r != left)
        })
    })
}

/// Pick the one release to install from the matches at a single version (#6771).
///
/// Why: with alias spellings accepted, one version can appear under two tags.
/// Agreement makes the pick safe; disagreement must surface as an error naming
/// BOTH tags rather than a coin flip.
///
/// # Postconditions
/// On `Ok`, the returned tag is one of `matches`' tags and its version equals
/// theirs. On `Err`, the message names every tag involved.
///
/// What: returns the lowest-ranked (most preferred) spelling when every match
/// agrees with it; otherwise a TAG-SPLIT error listing the tags.
///
/// Test: `tests::pinned_tag_resolves_when_both_spellings_agree`,
/// `tests::pinned_tag_errors_when_spellings_carry_different_digests`.
fn pick_agreeing(
    matches: &[(usize, Version, &GhRelease)],
    crate_name: &str,
) -> Result<ResolvedTag, String> {
    let Some((_, version, best)) = matches.iter().min_by_key(|(rank, _, _)| *rank) else {
        return Err(format!("no stable {crate_name}-v* release found"));
    };
    if let Some((_, _, other)) = matches.iter().find(|(_, _, r)| !releases_agree(best, r)) {
        let mut tags: Vec<&str> = matches
            .iter()
            .map(|(_, _, r)| r.tag_name.as_str())
            .collect();
        tags.sort_unstable();
        tags.dedup();
        return Err(format!(
            "TAG-SPLIT: {crate_name} {version} is published under more than one tag and they \
             disagree — {} names a different commit or asset digest than {}; tags involved: {}. \
             Nothing was installed; resolve the duplicate release before retrying.",
            other.tag_name,
            best.tag_name,
            tags.join(", ")
        ));
    }
    Ok(ResolvedTag {
        tag: best.tag_name.clone(),
        version: version.to_string(),
    })
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
    // #6771: match every tag spelling for this crate, not just its own name.
    let all = stable_matches(releases, crate_name);
    let Some(highest) = all.iter().map(|(_, v, _)| v).max().cloned() else {
        return Err(anyhow!("no stable {crate_name}-v* release found"));
    };
    let at_highest: Vec<_> = all.into_iter().filter(|(_, v, _)| *v == highest).collect();
    pick_agreeing(&at_highest, crate_name).map_err(|msg| anyhow!(msg))
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
    /// The version is published under two disagreeing tag spellings (#6771).
    #[error("{0}")]
    TagSplit(String),
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
/// `tests::select_exact_version_ignores_other_crates`,
/// `tests::select_exact_version_skips_prerelease`,
/// `tests::select_exact_version_rejects_non_semver_pin`.
fn select_exact_version(
    releases: &[GhRelease],
    crate_name: &str,
    version: &str,
) -> Result<ResolvedTag, ResolveError> {
    // Normalise through semver so `2.9.4` and a tag written `2.9.4` compare by
    // value rather than by string — and so a caller-supplied non-semver pin is
    // rejected here rather than 404ing later against a URL built from garbage.
    let wanted = Version::parse(version).ok();
    // #6771: a `tga` pin also matches `trusty-git-analytics-v*`, and the
    // published-version list below spans both spellings.
    let all = stable_matches(releases, crate_name);

    let matched: Vec<_> = all
        .iter()
        .filter(|(_, ver, _)| wanted.as_ref() == Some(ver))
        .cloned()
        .collect();
    if !matched.is_empty() {
        return pick_agreeing(&matched, crate_name).map_err(ResolveError::TagSplit);
    }

    let mut available: Vec<Version> = all.into_iter().map(|(_, ver, _)| ver).collect();
    available.sort();
    available.dedup();
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
            target_commitish: None,
            assets: Vec::new(),
        }
    }

    /// A release carrying the identity fields the #6771 split check reads.
    fn tga_release(tag: &str, commit: &str, digest: &str) -> GhRelease {
        GhRelease {
            tag_name: tag.to_owned(),
            prerelease: false,
            target_commitish: Some(commit.to_owned()),
            assets: vec![GhAsset {
                name: "trusty-git-analytics-7.0.0-aarch64-apple-darwin.tar.gz".to_owned(),
                digest: Some(digest.to_owned()),
            }],
        }
    }

    const COMMIT_A: &str = "0123456789abcdef0123456789abcdef01234567";
    const COMMIT_B: &str = "fedcba9876543210fedcba9876543210fedcba98";
    const DIGEST_A: &str = "sha256:aaaa";
    const DIGEST_B: &str = "sha256:bbbb";

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

    /// Why: A `tga` pin must accept the tag the publish gate actually pushes,
    /// which is derived from the crate DIRECTORY — this is #6771 verbatim, the
    /// case that made `taudit install` report tga 7.0.0 as unpublished.
    /// What: Publishes only `trusty-git-analytics-v7.0.0`; pins `tga` 7.0.0;
    /// asserts the directory-name tag resolves.
    /// Test: This is the test.
    #[test]
    fn pinned_tag_resolves_from_directory_name_tag() {
        let releases = vec![release("trusty-git-analytics-v7.0.0", false)];
        let rt = select_exact_version(&releases, "tga", "7.0.0").expect("alias tag must resolve");
        assert_eq!(rt.tag, "trusty-git-analytics-v7.0.0");
        assert_eq!(rt.version, "7.0.0");
    }

    /// Why: The package-name spelling stayed valid — earlier tga releases are
    /// tagged `tga-v*` and must keep resolving.
    /// What: Publishes only `tga-v7.0.0`; asserts it resolves.
    /// Test: This is the test.
    #[test]
    fn pinned_tag_resolves_from_package_name_tag() {
        let releases = vec![release("tga-v7.0.0", false)];
        let rt = select_exact_version(&releases, "tga", "7.0.0").expect("own tag must resolve");
        assert_eq!(rt.tag, "tga-v7.0.0");
    }

    /// Why: The mitigation for #6771 pushed an alias tag at the same commit, so
    /// both spellings exist for tga 7.0.0; agreeing tags must resolve rather
    /// than trip the split guard.
    /// What: Publishes both tags at one commit with one digest; asserts the
    /// package-name spelling (first candidate) wins.
    /// Test: This is the test.
    #[test]
    fn pinned_tag_resolves_when_both_spellings_agree() {
        let releases = vec![
            tga_release("trusty-git-analytics-v7.0.0", COMMIT_A, DIGEST_A),
            tga_release("tga-v7.0.0", COMMIT_A, DIGEST_A),
        ];
        let rt = select_exact_version(&releases, "tga", "7.0.0").expect("agreeing tags resolve");
        assert_eq!(rt.tag, "tga-v7.0.0");
    }

    /// Why: Two tags at one version naming different artifacts must never be
    /// silently picked between — the installer would ship an artifact nobody
    /// pinned.
    /// What: Publishes both spellings at different commits with different asset
    /// digests; asserts a TAG-SPLIT error naming BOTH tags.
    /// Test: This is the test.
    #[test]
    fn pinned_tag_errors_when_spellings_carry_different_digests() {
        let releases = vec![
            tga_release("tga-v7.0.0", COMMIT_A, DIGEST_A),
            tga_release("trusty-git-analytics-v7.0.0", COMMIT_B, DIGEST_B),
        ];
        let err = select_exact_version(&releases, "tga", "7.0.0")
            .expect_err("disagreeing tags must not resolve");
        let ResolveError::TagSplit(detail) = err else {
            panic!("expected TagSplit, got {err:?}");
        };
        assert!(detail.contains("tga-v7.0.0"), "got: {detail}");
        assert!(
            detail.contains("trusty-git-analytics-v7.0.0"),
            "got: {detail}"
        );
    }

    /// Why: #6771's closure condition 2 — the "not a published stable release"
    /// message must list what IS published under either spelling, or it tells
    /// the operator a released version does not exist.
    /// What: Publishes 6.0.0 as `tga-v*` and 7.0.0 as `trusty-git-analytics-v*`;
    /// pins 9.9.9; asserts both versions are reported.
    /// Test: This is the test.
    #[test]
    fn not_published_lists_versions_from_both_spellings() {
        let releases = vec![
            release("tga-v6.0.0", false),
            release("trusty-git-analytics-v7.0.0", false),
        ];
        let err = select_exact_version(&releases, "tga", "9.9.9").expect_err("9.9.9 is absent");
        match err {
            ResolveError::NotPublished { available } => {
                assert_eq!(available, vec!["6.0.0".to_owned(), "7.0.0".to_owned()]);
            }
            other => panic!("expected NotPublished, got {other:?}"),
        }
    }

    /// Why: The `latest` path resolves the same crate and must see the same
    /// releases the pinned path does.
    /// What: Publishes 6.0.0 under one spelling and 7.0.0 under the other;
    /// asserts the highest wins with its own tag.
    /// Test: This is the test.
    #[test]
    fn latest_tag_spans_both_spellings() {
        let releases = vec![
            release("tga-v6.0.0", false),
            release("trusty-git-analytics-v7.0.0", false),
        ];
        let rt = select_highest_semver(&releases, "tga").expect("7.0.0 must resolve");
        assert_eq!(rt.tag, "trusty-git-analytics-v7.0.0");
        assert_eq!(rt.version, "7.0.0");
    }

    /// Why: The candidate table is the whole fix; pin both directions so a
    /// `tga` pin and a `trusty-git-analytics` pin resolve the same releases.
    /// What: Asserts both spellings are offered for either input.
    /// Test: This is the test.
    #[test]
    fn tag_name_candidates_covers_both_tga_spellings() {
        assert_eq!(tag_name_candidates("tga"), ["tga", "trusty-git-analytics"]);
        assert_eq!(
            tag_name_candidates("trusty-git-analytics"),
            ["trusty-git-analytics", "tga"]
        );
    }

    /// Why: Every crate without an alias must resolve exactly as before.
    /// What: Asserts unaliased crate names yield only themselves.
    /// Test: This is the test.
    #[test]
    fn tag_name_candidates_defaults_to_crate_name() {
        for c in ["trusty-search", "trusty-memory", "trusty-installer"] {
            assert_eq!(tag_name_candidates(c), [c]);
        }
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
