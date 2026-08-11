use super::*;
use std::collections::HashMap;
use std::sync::Arc;

/// Routes a fixture server answers: path → (status code, body bytes).
type Routes = HashMap<String, (u16, Vec<u8>)>;

/// Serve fixed responses per PATH on loopback, for as long as the test runs.
///
/// Why: Every fail-closed arm must be provable WITHOUT a live network call.
/// The crate's existing `commands::test_support` stubs answer a fixed
/// SEQUENCE with JSON bodies; this pipeline issues three requests and one of
/// them is a binary tarball, so it needs path routing and byte bodies. Same
/// raw-`TcpListener` vehicle, no new dev-dependency.
///
/// What: Binds an ephemeral loopback port, spawns an accept loop, and
/// answers each request from `routes` (404 for anything unrouted). Returns
/// the base URL.
async fn serve_fixture(routes: Routes) -> String {
    let routes = Arc::new(routes);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let routes = Arc::clone(&routes);
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                // Drain the header block before replying — the split-request
                // flake shape `test_support::serve_fixed` documents.
                let mut acc = Vec::with_capacity(2048);
                let mut chunk = [0u8; 2048];
                loop {
                    match sock.read(&mut chunk).await {
                        Ok(0) => break,
                        Ok(n) => {
                            acc.extend_from_slice(&chunk[..n]);
                            if acc.windows(4).any(|w| w == b"\r\n\r\n") {
                                break;
                            }
                        }
                        Err(_) => return,
                    }
                }
                let head = String::from_utf8_lossy(&acc);
                let path = head
                    .lines()
                    .next()
                    .and_then(|l| l.split_whitespace().nth(1))
                    .unwrap_or("/")
                    .to_owned();
                let (status, body) = match routes.get(&path) {
                    Some((s, b)) => (*s, b.clone()),
                    None => (404, b"not found".to_vec()),
                };
                let reason = if status == 200 { "OK" } else { "Not Found" };
                let resp = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.write_all(&body).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    format!("http://{addr}")
}

/// A `.tar.gz` holding one executable that prints `version_line`.
///
/// Why: Check 5 probes the staged binary with `--version`, so the fixture
/// must be genuinely executable — a byte blob would prove nothing about the
/// version gate.
fn fake_tarball(binary: &str, version_line: &str) -> Vec<u8> {
    use flate2::{write::GzEncoder, Compression};
    let script = format!("#!/bin/sh\necho '{version_line}'\n");
    let data = script.as_bytes();
    let enc = GzEncoder::new(Vec::new(), Compression::fast());
    let mut ar = tar::Builder::new(enc);
    let mut header = tar::Header::new_gnu();
    header.set_size(data.len() as u64);
    header.set_mode(0o755);
    header.set_cksum();
    ar.append_data(&mut header, binary, data).unwrap();
    ar.into_inner().unwrap().finish().unwrap()
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

/// The asset suffix the code under test will pick for this host.
fn suffix_for(crate_name: &str, target: &str) -> String {
    glibc::select_asset_suffix(crate_name, target, glibc::host_glibc_version()).suffix
}

/// How a fixture release deviates from a valid one.
#[derive(Clone, Copy, PartialEq)]
enum Flaw {
    /// A correct, installable release.
    None,
    /// Tag published, but the asset 404s.
    AssetMissing,
    /// Asset served, but its published checksum names other bytes.
    BadChecksum,
    /// Asset and checksum fine, but the binary reports another version.
    WrongBinaryVersion,
}

/// Builds the releases JSON and asset routes for a set of fixture releases.
struct Fixture {
    target: &'static str,
    tags: Vec<String>,
    routes: Routes,
}

impl Fixture {
    fn new(target: &'static str) -> Self {
        Self {
            target,
            tags: Vec::new(),
            routes: HashMap::new(),
        }
    }

    /// Publish `crate_name` at `version`, deviating per `flaw`.
    fn publish(mut self, crate_name: &str, version: &str, flaw: Flaw) -> Self {
        self.tags.push(format!("{crate_name}-v{version}"));
        if flaw == Flaw::AssetMissing {
            return self;
        }
        let reported = if flaw == Flaw::WrongBinaryVersion {
            "9.9.9"
        } else {
            version
        };
        let tarball = fake_tarball(crate_name, &format!("{crate_name} {reported}"));
        let digest = if flaw == Flaw::BadChecksum {
            sha256_hex(b"entirely different bytes")
        } else {
            sha256_hex(&tarball)
        };
        let file =
            release::asset_filename(crate_name, version, &suffix_for(crate_name, self.target));
        let key = format!("/dl/{crate_name}-v{version}/{file}");
        self.routes.insert(
            format!("{key}.sha256"),
            (200, format!("{digest}  {file}\n").into_bytes()),
        );
        self.routes.insert(key, (200, tarball));
        self
    }

    /// The SHA-256 of the artifact published for `crate_name` at `version`.
    fn digest_of(&self, crate_name: &str, version: &str) -> String {
        let file =
            release::asset_filename(crate_name, version, &suffix_for(crate_name, self.target));
        let key = format!("/dl/{crate_name}-v{version}/{file}");
        sha256_hex(&self.routes.get(&key).expect("published asset").1)
    }

    async fn start(mut self) -> String {
        let json = format!(
            "[{}]",
            self.tags
                .iter()
                .map(|t| format!(r#"{{"tag_name":"{t}","prerelease":false}}"#))
                .collect::<Vec<_>>()
                .join(",")
        );
        self.routes
            .insert("/releases".to_owned(), (200, json.into_bytes()));
        serve_fixture(self.routes).await
    }
}

/// Drive the pinned entry point against a fixture, into `install_dir`.
async fn run(
    base: &str,
    tools: &[PinnedTool],
    install_dir: &Path,
) -> Result<Vec<PinnedInstall>, PinnedError> {
    let releases_url = format!("{base}/releases");
    let download_base = format!("{base}/dl");
    let endpoints = Endpoints {
        releases_url: &releases_url,
        download_base: &download_base,
    };
    install_pinned_set_at(&reqwest::Client::new(), &endpoints, tools, install_dir).await
}

/// Assert nothing landed in the install directory — the "nothing was
/// installed" half of every fail-closed claim.
fn assert_nothing_installed(install_dir: &Path) {
    let found: Vec<String> = std::fs::read_dir(install_dir)
        .map(|d| {
            d.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        found.is_empty(),
        "fail-closed violated: {} contains {found:?}",
        install_dir.display()
    );
}

/// The host target, or `None` when this host publishes no prebuilts.
fn tier1() -> Option<&'static str> {
    platform::current_target()
}

/// Why: A pin is normally proved by the binary of the same name; requiring
/// callers to repeat it would be noise.
/// What: Asserts the default and the override.
/// Test: This is the test.
#[test]
fn pinned_tool_defaults_binary_to_crate_name() {
    let t = PinnedTool::new("trusty-review", "0.9.1");
    assert_eq!(t.binary, "trusty-review");
    assert_eq!(t.version, "0.9.1");
    assert!(t.sha256.is_none());
    assert_eq!(t.with_binary("tr").binary, "tr");
}

/// Why: The happy path must actually install, or the fail-closed tests below
/// could pass for the trivial reason that nothing ever installs.
/// What: Publishes a valid release, pins it, asserts the binary lands and the
/// reported version equals the pin.
/// Test: This is the test.
#[tokio::test]
async fn pinned_install_places_and_reports_the_pinned_version() {
    let Some(target) = tier1() else { return };
    let base = Fixture::new(target)
        .publish("demo-tool", "1.2.3", Flaw::None)
        .start()
        .await;
    let dir = tempfile::tempdir().unwrap();

    let installed = run(&base, &[PinnedTool::new("demo-tool", "1.2.3")], dir.path())
        .await
        .expect("a valid pinned release must install");

    assert_eq!(installed.len(), 1);
    assert_eq!(installed[0].version, "1.2.3");
    assert!(dir.path().join("demo-tool").exists());
}

/// Why: THE headline guarantee. `try_install_prebuilt` resolves `latest`, so
/// pinning a version that was never published would silently install the
/// newest one. Here a newer release EXISTS and must not be substituted.
/// What: Publishes 1.2.3 only; pins 1.0.0. Asserts `VersionNotPublished`
/// listing what is published, and an empty install dir.
/// Test: This is the test.
#[tokio::test]
async fn fails_closed_when_the_pinned_version_was_never_published() {
    let Some(target) = tier1() else { return };
    let base = Fixture::new(target)
        .publish("demo-tool", "1.2.3", Flaw::None)
        .start()
        .await;
    let dir = tempfile::tempdir().unwrap();

    let err = run(&base, &[PinnedTool::new("demo-tool", "1.0.0")], dir.path())
        .await
        .expect_err("an unpublished pin must fail closed, never resolve to latest");

    match &err {
        PinnedError::VersionNotPublished {
            version, available, ..
        } => {
            assert_eq!(version, "1.0.0");
            assert_eq!(available, &vec!["1.2.3".to_owned()]);
        }
        other => panic!("expected VersionNotPublished, got {other:?}"),
    }
    assert!(err.to_string().contains("nothing was installed"));
    assert_nothing_installed(dir.path());
}

/// Why: A tampered or corrupted artifact must never be installed, and must
/// never degrade into a source build.
/// What: Publishes an asset whose `.sha256` names other bytes; asserts
/// `ChecksumMismatch` carrying both digests, and an empty install dir.
/// Test: This is the test.
#[tokio::test]
async fn fails_closed_on_a_checksum_mismatch() {
    let Some(target) = tier1() else { return };
    let base = Fixture::new(target)
        .publish("demo-tool", "1.2.3", Flaw::BadChecksum)
        .start()
        .await;
    let dir = tempfile::tempdir().unwrap();

    let err = run(&base, &[PinnedTool::new("demo-tool", "1.2.3")], dir.path())
        .await
        .expect_err("a checksum mismatch must fail closed");

    match &err {
        PinnedError::ChecksumMismatch {
            expected, actual, ..
        } => {
            assert_ne!(expected, actual, "the test fixture must actually disagree");
        }
        other => panic!("expected ChecksumMismatch, got {other:?}"),
    }
    assert!(err.to_string().contains("nothing was installed"));
    assert_nothing_installed(dir.path());
}

/// Why: A caller-pinned digest is what makes a re-uploaded asset at the same
/// tag detectable — the published sidecar would agree with the new bytes.
/// What: Publishes a valid release, pins a DIFFERENT digest; asserts
/// `PinnedChecksumMismatch` and an empty install dir.
/// Test: This is the test.
#[tokio::test]
async fn fails_closed_when_the_artifact_does_not_match_a_caller_pinned_digest() {
    let Some(target) = tier1() else { return };
    let base = Fixture::new(target)
        .publish("demo-tool", "1.2.3", Flaw::None)
        .start()
        .await;
    let dir = tempfile::tempdir().unwrap();
    let wrong = "0".repeat(64);

    let err = run(
        &base,
        &[PinnedTool::new("demo-tool", "1.2.3").with_sha256(&wrong)],
        dir.path(),
    )
    .await
    .expect_err("a caller-pinned digest mismatch must fail closed");

    match &err {
        PinnedError::PinnedChecksumMismatch { pinned, actual, .. } => {
            assert_eq!(pinned, &wrong);
            assert_ne!(actual, &wrong);
        }
        other => panic!("expected PinnedChecksumMismatch, got {other:?}"),
    }
    assert_nothing_installed(dir.path());
}

/// Why: The matching-digest case must still install, or the test above would
/// pass because caller digests are simply always rejected.
/// What: Pins the artifact's real digest; asserts it installs.
/// Test: This is the test.
#[tokio::test]
async fn a_matching_caller_pinned_digest_installs() {
    let Some(target) = tier1() else { return };
    let fixture = Fixture::new(target).publish("demo-tool", "1.2.3", Flaw::None);
    let digest = fixture.digest_of("demo-tool", "1.2.3");
    let base = fixture.start().await;
    let dir = tempfile::tempdir().unwrap();

    let installed = run(
        &base,
        &[PinnedTool::new("demo-tool", "1.2.3").with_sha256(&digest)],
        dir.path(),
    )
    .await
    .expect("a matching pinned digest must install");

    assert_eq!(installed[0].version, "1.2.3");
}

/// Why: A published tag whose asset is missing (a release leg that failed, or
/// a platform never built) must be terminal — this is precisely where
/// `try_install_prebuilt` 404s and silently builds from source instead.
/// What: Publishes the tag with no asset routes; asserts `ArtifactUnavailable`
/// naming the URL, and an empty install dir.
/// Test: This is the test.
#[tokio::test]
async fn fails_closed_when_the_artifact_is_unavailable() {
    let Some(target) = tier1() else { return };
    let base = Fixture::new(target)
        .publish("demo-tool", "1.2.3", Flaw::AssetMissing)
        .start()
        .await;
    let dir = tempfile::tempdir().unwrap();

    let err = run(&base, &[PinnedTool::new("demo-tool", "1.2.3")], dir.path())
        .await
        .expect_err("a missing artifact must fail closed, never fall back");

    match &err {
        PinnedError::ArtifactUnavailable { url, .. } => {
            assert!(url.contains("demo-tool-v1.2.3"), "unexpected url: {url}");
        }
        other => panic!("expected ArtifactUnavailable, got {other:?}"),
    }
    assert!(err.to_string().contains("nothing was installed"));
    assert_nothing_installed(dir.path());
}

/// Why: Every URL-level check can pass while the artifact still contains the
/// wrong build — a mis-tagged release. Only probing the binary catches it,
/// and probing it in staging is what keeps the failure from installing.
/// What: Serves a correctly-named, correctly-checksummed asset whose binary
/// reports 9.9.9; asserts `VersionMismatch` and an empty install dir.
/// Test: This is the test.
#[tokio::test]
async fn fails_closed_when_the_downloaded_binary_reports_another_version() {
    let Some(target) = tier1() else { return };
    let base = Fixture::new(target)
        .publish("demo-tool", "1.2.3", Flaw::WrongBinaryVersion)
        .start()
        .await;
    let dir = tempfile::tempdir().unwrap();

    let err = run(&base, &[PinnedTool::new("demo-tool", "1.2.3")], dir.path())
        .await
        .expect_err("a binary reporting another version must fail closed");

    match &err {
        PinnedError::VersionMismatch {
            pinned, reported, ..
        } => {
            assert_eq!(pinned, "1.2.3");
            assert_eq!(reported, "9.9.9");
        }
        other => panic!("expected VersionMismatch, got {other:?}"),
    }
    assert!(err.to_string().contains("nothing was installed"));
    assert_nothing_installed(dir.path());
}

/// Why: The archive must contain the binary that proves the pin; otherwise
/// the version gate would be silently skipped.
/// What: Pins a binary name absent from the archive; asserts `BinaryMissing`
/// and an empty install dir.
/// Test: This is the test.
#[tokio::test]
async fn fails_closed_when_the_archive_lacks_the_binary_that_proves_the_pin() {
    let Some(target) = tier1() else { return };
    let base = Fixture::new(target)
        .publish("demo-tool", "1.2.3", Flaw::None)
        .start()
        .await;
    let dir = tempfile::tempdir().unwrap();

    let err = run(
        &base,
        &[PinnedTool::new("demo-tool", "1.2.3").with_binary("not-in-archive")],
        dir.path(),
    )
    .await
    .expect_err("a missing binary must fail closed");

    assert!(
        matches!(err, PinnedError::BinaryMissing { .. }),
        "got {err:?}"
    );
    assert_nothing_installed(dir.path());
}

/// Why: The multi-tool guarantee `trusty-audit` depends on — a set that
/// half-installs would let an audit run with one pinned tool and one stale
/// one, which is the mixed-version result pinning exists to prevent.
/// What: Two tools, the FIRST fully valid and the second's asset missing.
/// Asserts the error is the second tool's AND the first tool's binary was
/// never placed, despite having verified successfully.
/// Test: This is the test.
#[tokio::test]
async fn a_set_installs_nothing_when_any_tool_fails() {
    let Some(target) = tier1() else { return };
    let base = Fixture::new(target)
        .publish("good-tool", "1.0.0", Flaw::None)
        .publish("bad-tool", "2.0.0", Flaw::AssetMissing)
        .start()
        .await;
    let dir = tempfile::tempdir().unwrap();

    let err = run(
        &base,
        &[
            PinnedTool::new("good-tool", "1.0.0"),
            PinnedTool::new("bad-tool", "2.0.0"),
        ],
        dir.path(),
    )
    .await
    .expect_err("one failing tool must fail the whole set");

    assert!(
        matches!(err, PinnedError::ArtifactUnavailable { .. }),
        "got {err:?}"
    );
    assert!(
        !dir.path().join("good-tool").exists(),
        "all-or-nothing violated: the verified tool was installed anyway"
    );
    assert_nothing_installed(dir.path());
}

/// Why: The set path must install every tool when all of them verify — the
/// positive half of all-or-nothing.
/// What: Two valid tools; asserts both land at their pinned versions.
/// Test: This is the test.
#[tokio::test]
async fn a_set_installs_every_tool_when_all_verify() {
    let Some(target) = tier1() else { return };
    let base = Fixture::new(target)
        .publish("tool-a", "1.0.0", Flaw::None)
        .publish("tool-b", "2.0.0", Flaw::None)
        .start()
        .await;
    let dir = tempfile::tempdir().unwrap();

    let installed = run(
        &base,
        &[
            PinnedTool::new("tool-a", "1.0.0"),
            PinnedTool::new("tool-b", "2.0.0"),
        ],
        dir.path(),
    )
    .await
    .expect("all valid tools must install");

    assert_eq!(installed.len(), 2);
    assert_eq!(installed[0].version, "1.0.0");
    assert_eq!(installed[1].version, "2.0.0");
    assert!(dir.path().join("tool-a").exists());
    assert!(dir.path().join("tool-b").exists());
}

/// Why: An unreachable release list must not be mistaken for "not published",
/// because the two call for different operator action.
/// What: Points the resolver at a port that refuses; asserts
/// `ReleaseLookupFailed`, not `VersionNotPublished`.
/// Test: This is the test.
#[tokio::test]
async fn fails_closed_when_the_release_list_is_unreachable() {
    if tier1().is_none() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    // The crate's existing guaranteed-to-refuse loopback address.
    let base = format!("http://{}", crate::commands::test_support::dead_addr());
    let err = run(&base, &[PinnedTool::new("demo-tool", "1.2.3")], dir.path())
        .await
        .expect_err("an unreachable release list must fail closed");

    assert!(
        matches!(err, PinnedError::ReleaseLookupFailed { .. }),
        "got {err:?}"
    );
    assert_nothing_installed(dir.path());
}
