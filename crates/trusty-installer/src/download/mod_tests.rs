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
    )
    .await;

    assert!(
        matches!(outcome, Outcome::Fallback { .. }),
        "an absent asset stays a routine fallback, got {outcome:?}"
    );
}
