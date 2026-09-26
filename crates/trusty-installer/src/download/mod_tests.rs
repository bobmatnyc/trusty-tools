//! #5518 — a SHA-256 mismatch must reach the operator AS a SHA-256 mismatch.
//!
//! Why: `Outcome::Fallback` used to carry both "no prebuilt for this platform"
//! and "the bytes we downloaded are not the bytes the release published". The
//! second is the one condition the checksum exists to detect, and flattening it
//! into the first meant a tampered download surfaced to the user as a slower
//! install with no warning. Every test here asserts the DIRECTION of that fix:
//! the two conditions produce different outcomes AND different operator-facing
//! text.
//!
//! What: The download+verify leg runs against a loopback fixture and needs no
//! Tier-1 host, because [`super::install_from_urls`] takes explicit URLs. Only
//! the whole-orchestrator test needs a host that publishes prebuilts, since tag
//! resolution runs before the download.
//!
//! Test: This is the test module.

use super::test_fixture::{fake_tarball, serve_fixture, sha256_hex, Routes};
use super::*;

/// A release whose asset is served with a `.sha256` naming `published_digest`.
///
/// Why: A mismatch is constructed by publishing a digest for bytes other than
/// the ones served — exactly what a swapped artifact or a truncated transfer
/// looks like from the client's side.
///
/// What: Returns `(base_url, archive_name, tarball_bytes)`.
async fn serve_release(archive: &str, tarball: Vec<u8>, published_digest: &str) -> String {
    let mut routes: Routes = std::collections::HashMap::new();
    routes.insert(
        format!("/dl/{archive}.sha256"),
        (200, format!("{published_digest}  {archive}\n").into_bytes()),
    );
    routes.insert(format!("/dl/{archive}"), (200, tarball));
    serve_fixture(routes).await
}

/// Why: THE regression. Before #5518 this arm returned an
/// `anyhow::Error` string that `install_from_urls` wrapped with `.context()`
/// and the orchestrator flattened into a fallback reason.
/// What: Serves an asset whose published checksum names other bytes; asserts
/// the typed [`fetch::DownloadError::ChecksumMismatch`] carrying both digests.
/// Test: This is the test.
#[tokio::test]
async fn download_and_verify_reports_a_mismatch_as_a_mismatch() {
    let archive = "demo-tool-1.2.3.tar.gz";
    let tarball = fake_tarball("demo-tool", "demo-tool 1.2.3");
    let real = sha256_hex(&tarball);
    let lie = sha256_hex(b"entirely different bytes");
    let base = serve_release(archive, tarball, &lie).await;
    let tmp = tempfile::tempdir().unwrap();

    let err = fetch::download_and_verify(
        &http_client(),
        &format!("{base}/dl/{archive}"),
        &format!("{base}/dl/{archive}.sha256"),
        archive,
        tmp.path(),
    )
    .await
    .expect_err("bytes that do not match their published digest must not verify");

    match err {
        fetch::DownloadError::ChecksumMismatch {
            archive: a,
            expected,
            actual,
        } => {
            assert_eq!(a, archive);
            assert_eq!(
                expected, lie,
                "must report the digest the release published"
            );
            assert_eq!(actual, real, "must report the digest of the bytes received");
        }
        other => panic!("a checksum mismatch must not be an opaque error: {other:?}"),
    }
}

/// Why: The other half of the distinction — an absent asset must stay a routine
/// `Other`, or making the mismatch loud would just make every 404 loud too.
/// What: Serves no asset at all; asserts `Other`, not `ChecksumMismatch`.
/// Test: This is the test.
#[tokio::test]
async fn download_and_verify_reports_a_missing_asset_as_other() {
    let base = serve_fixture(std::collections::HashMap::new()).await;
    let tmp = tempfile::tempdir().unwrap();

    let err = fetch::download_and_verify(
        &http_client(),
        &format!("{base}/dl/absent.tar.gz"),
        &format!("{base}/dl/absent.tar.gz.sha256"),
        "absent.tar.gz",
        tmp.path(),
    )
    .await
    .expect_err("a 404 asset cannot verify");

    assert!(
        matches!(err, fetch::DownloadError::Other(_)),
        "a missing asset is not an integrity failure, got {err:?}"
    );
}

/// Why: Guards against the mismatch test passing for the trivial reason that
/// nothing ever verifies.
/// What: Publishes the true digest; asserts the tarball verifies and lands.
/// Test: This is the test.
#[tokio::test]
async fn download_and_verify_accepts_matching_bytes() {
    let archive = "demo-tool-1.2.3.tar.gz";
    let tarball = fake_tarball("demo-tool", "demo-tool 1.2.3");
    let digest = sha256_hex(&tarball);
    let base = serve_release(archive, tarball, &digest).await;
    let tmp = tempfile::tempdir().unwrap();

    let path = fetch::download_and_verify(
        &http_client(),
        &format!("{base}/dl/{archive}"),
        &format!("{base}/dl/{archive}.sha256"),
        archive,
        tmp.path(),
    )
    .await
    .expect("bytes matching their published digest must verify");
    assert!(path.exists());
}

/// Why: The abort-vs-fall-back decision lives in [`classify`], and #5518 is a
/// defect in exactly that mapping. A new variant every caller treats alike
/// would not be a fix, so this asserts the two inputs produce two outcomes.
/// What: Feeds `classify` a mismatch and a routine failure; asserts distinct
/// variants, and that only the routine one offers a source build.
/// Test: This is the test.
#[test]
fn a_mismatch_and_an_absent_prebuilt_are_different_outcomes() {
    let mismatch = classify(
        "trusty-search",
        "0.46.0",
        "trusty-search-0.46.0.tar.gz",
        "https://example.invalid/trusty-search-0.46.0.tar.gz",
        Err(fetch::DownloadError::ChecksumMismatch {
            archive: "trusty-search-0.46.0.tar.gz".to_owned(),
            expected: "a".repeat(64),
            actual: "b".repeat(64),
        }),
    );
    let absent = classify(
        "trusty-search",
        "0.46.0",
        "trusty-search-0.46.0.tar.gz",
        "https://example.invalid/trusty-search-0.46.0.tar.gz",
        Err(fetch::DownloadError::Other(anyhow::anyhow!(
            "HTTP error from https://example.invalid: 404 Not Found"
        ))),
    );

    let Outcome::ChecksumMismatch(m) = &mismatch else {
        panic!("a failed checksum must not be a fallback: {mismatch:?}")
    };
    let Outcome::Fallback { reason } = &absent else {
        panic!("a 404 must stay a fallback: {absent:?}")
    };

    // The distinction has to reach a human, not just a type.
    assert!(
        reason.contains("cargo install"),
        "the routine path still offers a source build: {reason}"
    );
    assert!(
        !m.to_string().contains("falling back to cargo install"),
        "a tamper signal must never read as a routine fallback: {m}"
    );
}

/// Why: The rule the issue turns on — the mismatch must be surfaced AS a
/// mismatch. A variant a human never sees would satisfy the type and not the
/// requirement.
/// What: Asserts the operator-facing text names the condition, both digests,
/// the artifact, and that nothing was installed; and that it does not present
/// itself as an unavailable prebuilt.
/// Test: This is the test.
#[test]
fn mismatch_message_names_both_digests_and_never_offers_a_source_build() {
    let m = ChecksumMismatch {
        crate_name: "trusty-search".to_owned(),
        version: "0.46.0".to_owned(),
        archive: "trusty-search-0.46.0-aarch64-apple-darwin.tar.gz".to_owned(),
        url: "https://example.invalid/asset.tar.gz".to_owned(),
        expected: "a".repeat(64),
        actual: "b".repeat(64),
    };
    let text = m.to_string();

    for needle in [
        "checksum mismatch",
        "trusty-search",
        "0.46.0",
        &"a".repeat(64),
        &"b".repeat(64),
        "trusty-search-0.46.0-aarch64-apple-darwin.tar.gz",
        "https://example.invalid/asset.tar.gz",
        "Nothing was installed",
    ] {
        assert!(
            text.contains(needle),
            "message must name {needle:?}: {text}"
        );
    }
    assert!(
        !text.contains("prebuilt unavailable"),
        "a mismatch must not read as an absent prebuilt: {text}"
    );
}

/// The host target, or `None` when this host publishes no prebuilts.
fn tier1() -> Option<&'static str> {
    platform::current_target()
}

/// Why: End-to-end proof through the real entry point — tag resolution, asset
/// selection, download, verify — so the fix is not only true of the pieces.
/// What: Publishes one release whose `.sha256` names other bytes; asserts
/// `try_install_prebuilt_at` returns `ChecksumMismatch` and installs nothing.
/// Test: This is the test. Skipped on a host with no Tier-1 target, where the
/// orchestrator returns `Fallback` before any network call — the same guard
/// `pinned::tests` uses.
#[tokio::test]
async fn a_checksum_mismatch_is_reported_as_a_checksum_mismatch() {
    let Some(target) = tier1() else { return };
    let (name, version) = ("demo-tool", "1.2.3");
    let suffix = glibc::select_asset_suffix(name, target, glibc::host_glibc_version()).suffix;
    let archive = release::asset_filename(name, version, &suffix);
    let tarball = fake_tarball(name, &format!("{name} {version}"));
    let real = sha256_hex(&tarball);
    let lie = sha256_hex(b"entirely different bytes");

    let mut routes: Routes = std::collections::HashMap::new();
    routes.insert(
        "/releases".to_owned(),
        (
            200,
            format!(r#"[{{"tag_name":"{name}-v{version}","prerelease":false}}]"#).into_bytes(),
        ),
    );
    let key = format!("/dl/{name}-v{version}/{archive}");
    routes.insert(
        format!("{key}.sha256"),
        (200, format!("{lie}  {archive}\n").into_bytes()),
    );
    routes.insert(key, (200, tarball));
    let base = serve_fixture(routes).await;

    let releases_url = format!("{base}/releases");
    let download_base = format!("{base}/dl");
    let dir = tempfile::tempdir().unwrap();

    let outcome = try_install_prebuilt_at(
        &http_client(),
        &pinned::Endpoints {
            releases_url: &releases_url,
            download_base: &download_base,
        },
        name,
        dir.path(),
        None,
    )
    .await;

    let Outcome::ChecksumMismatch(m) = &outcome else {
        panic!("a tampered artifact must not degrade into a source build: {outcome:?}")
    };
    assert_eq!(m.crate_name, name);
    assert_eq!(m.version, version);
    assert_eq!(m.expected, lie);
    assert_eq!(m.actual, real);
    assert!(m.to_string().contains("checksum mismatch"));

    let landed: Vec<String> = std::fs::read_dir(dir.path())
        .map(|d| {
            d.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        landed.is_empty(),
        "nothing may be installed from an unverified artifact, found {landed:?}"
    );
}

/// Why: The distinction must not be bought by making every download failure
/// loud — an unbuilt platform is still a routine fallback.
/// What: Publishes a tag whose asset 404s; asserts `Fallback`.
/// Test: This is the test.
#[tokio::test]
async fn a_missing_asset_still_falls_back() {
    let Some(_) = tier1() else { return };
    let (name, version) = ("demo-tool", "1.2.3");
    let mut routes: Routes = std::collections::HashMap::new();
    routes.insert(
        "/releases".to_owned(),
        (
            200,
            format!(r#"[{{"tag_name":"{name}-v{version}","prerelease":false}}]"#).into_bytes(),
        ),
    );
    let base = serve_fixture(routes).await;

    let releases_url = format!("{base}/releases");
    let download_base = format!("{base}/dl");
    let dir = tempfile::tempdir().unwrap();

    let outcome = try_install_prebuilt_at(
        &http_client(),
        &pinned::Endpoints {
            releases_url: &releases_url,
            download_base: &download_base,
        },
        name,
        dir.path(),
        None,
    )
    .await;

    assert!(
        matches!(outcome, Outcome::Fallback { .. }),
        "an absent asset stays a routine fallback, got {outcome:?}"
    );
}

/// Why (#8642): `tctl upgrade` placed tga 8.0.0 over tga 8.0.0 and called it
/// an upgrade. A release not newer than the installed floor must fall back
/// BEFORE any byte is fetched or placed.
/// What: publishes only `demo-tool-v1.2.3` (with a valid asset, so only the
/// floor can stop it) and a floor of `1.2.3`; asserts `Fallback` naming both
/// versions, and an empty install dir.
/// Test: This is the test.
#[tokio::test]
async fn a_release_not_newer_than_the_floor_falls_back_before_download() {
    let Some(target) = tier1() else { return };
    let (name, version) = ("demo-tool", "1.2.3");
    let suffix = glibc::select_asset_suffix(name, target, glibc::host_glibc_version()).suffix;
    let archive = release::asset_filename(name, version, &suffix);
    let tarball = fake_tarball(name, &format!("{name} {version}"));
    let digest = sha256_hex(&tarball);

    let mut routes: Routes = std::collections::HashMap::new();
    routes.insert(
        "/releases".to_owned(),
        (
            200,
            format!(r#"[{{"tag_name":"{name}-v{version}","prerelease":false}}]"#).into_bytes(),
        ),
    );
    let key = format!("/dl/{name}-v{version}/{archive}");
    routes.insert(
        format!("{key}.sha256"),
        (200, format!("{digest}  {archive}\n").into_bytes()),
    );
    routes.insert(key, (200, tarball));
    let base = serve_fixture(routes).await;

    let releases_url = format!("{base}/releases");
    let download_base = format!("{base}/dl");
    let dir = tempfile::tempdir().unwrap();

    let outcome = try_install_prebuilt_at(
        &http_client(),
        &pinned::Endpoints {
            releases_url: &releases_url,
            download_base: &download_base,
        },
        name,
        dir.path(),
        Some(version),
    )
    .await;

    let Outcome::Fallback { reason } = &outcome else {
        panic!("a same-version prebuilt must not be placed as an upgrade: {outcome:?}")
    };
    assert!(
        reason.contains("not newer than installed 1.2.3"),
        "{reason}"
    );
    let landed = std::fs::read_dir(dir.path()).unwrap().count();
    assert_eq!(landed, 0, "nothing may be placed below the floor");
}

/// Why (#8642): the floor compares against INSTALLED, not crates.io latest —
/// a release newer than installed is an upgrade even if it lags latest.
/// What: equal and older releases produce a reason naming both versions; a
/// newer release, an absent floor, or an unparseable floor produce none.
/// Test: This is the test.
#[test]
fn floor_reason_names_both_versions() {
    let r = floor_fallback_reason("tga", "8.0.0", Some("8.0.0")).unwrap();
    assert!(r.contains("8.0.0") && r.contains("installed 8.0.0"), "{r}");
    assert!(floor_fallback_reason("tga", "7.9.0", Some("8.0.0")).is_some());
    assert!(floor_fallback_reason("tga", "9.0.1", Some("8.0.0")).is_none());
    assert!(floor_fallback_reason("tga", "8.0.0", None).is_none());
    assert!(floor_fallback_reason("tga", "8.0.0", Some("unknown")).is_none());
}

/// Why (#8642): live proof that tga resolves, downloads and verifies from its
/// own release repo, and that the floor does not block a real advance.
/// What: installs tga into a temp dir with floor 8.0.0 (the old repo's
/// newest); asserts `Installed` at >= 9.0.0 with a `tga` binary placed.
/// Test: `cargo test -p trusty-installer -- --include-ignored tga_prebuilt_live`.
#[tokio::test]
#[ignore = "performs a live GitHub download; run with --include-ignored"]
async fn tga_prebuilt_live_installs_past_the_old_repo_ceiling() {
    if tier1().is_none() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let outcome = try_install_prebuilt_with_floor("tga", dir.path(), Some("8.0.0")).await;
    let Outcome::Installed { paths, version } = &outcome else {
        panic!("tga must install from trusty-git-analytics: {outcome:?}")
    };
    let v = semver::Version::parse(version).unwrap();
    assert!(v >= semver::Version::new(9, 0, 0), "installed {version}");
    assert!(paths.iter().any(|p| p.ends_with("tga")), "{paths:?}");
}
