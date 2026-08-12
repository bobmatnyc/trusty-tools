use super::*;
// #5970: `pinned.rs` reaches the host-target probe through `preflight::resolve_pin`
// now, so it is no longer in scope via `use super::*`.
use crate::download::platform;
use std::collections::HashMap;

// #5518: one fixture server for the crate's download suites, not one per suite.
use crate::download::test_fixture::{fake_tarball, serve_fixture, sha256_hex, Routes};

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
    // #5495: the same constructor `trusty-audit` now calls, so this suite proves
    // the client the out-of-crate caller gets, not a lookalike built here.
    install_pinned_set_at(
        &crate::download::http_client(),
        &endpoints,
        tools,
        install_dir,
    )
    .await
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

/// Why: The all-or-nothing postcondition covers PLACEMENT too, not just
/// verification. A phase-2 failure used to return `Io` reading "nothing was
/// installed" while an earlier tool's binary sat in the install directory — a
/// false statement about on-disk state in a supply-chain path.
/// What: Two tools that both verify; a pre-existing DIRECTORY at the second
/// tool's destination makes its placement fail. Asserts the first tool's binary
/// is absent afterwards and the error still truthfully says nothing was
/// installed.
/// Test: This is the test.
#[tokio::test]
async fn a_set_places_nothing_when_a_later_tool_cannot_be_placed() {
    let Some(target) = tier1() else { return };
    let base = Fixture::new(target)
        .publish("good-tool", "1.0.0", Flaw::None)
        .publish("bad-place-tool", "2.0.0", Flaw::None)
        .start()
        .await;
    let dir = tempfile::tempdir().unwrap();
    // A directory cannot be replaced by `rename`, so the second tool's
    // placement fails while the first tool's has already succeeded.
    std::fs::create_dir(dir.path().join("bad-place-tool")).unwrap();

    let err = run(
        &base,
        &[
            PinnedTool::new("good-tool", "1.0.0"),
            PinnedTool::new("bad-place-tool", "2.0.0"),
        ],
        dir.path(),
    )
    .await
    .expect_err("an unplaceable tool must fail the whole set");

    assert!(
        !dir.path().join("good-tool").exists(),
        "all-or-nothing violated in phase 2: {err}"
    );
    assert!(
        err.to_string().contains("nothing was installed"),
        "an unplaceable set must report the truth: {err}"
    );
}

/// A `Staged` whose extraction dir holds one file per name in `binaries`.
fn staged_fixture(crate_name: &str, version: &str, binaries: &[&str], root: &Path) -> Staged {
    let extract_dir = root.join(crate_name);
    std::fs::create_dir_all(&extract_dir).unwrap();
    for b in binaries {
        std::fs::write(extract_dir.join(b), b"#!/bin/sh\nexit 0\n").unwrap();
    }
    Staged {
        tool: PinnedTool::new(crate_name, version),
        extract_dir,
        binary_names: binaries.iter().map(|b| (*b).to_owned()).collect(),
    }
}

/// Why: Same-directory renames are near-atomic but not infallible, so the
/// residual mid-commit failure must report the truth. The old code reused the
/// `Io` variant here, whose text says "nothing was installed" — false whenever an
/// earlier rename already succeeded.
/// What: Commits two tools where the second's temporary file does not exist, so
/// its rename fails after the first has committed. Asserts the distinct variant,
/// the files and crates it names, and that it never claims a clean directory.
/// Test: This is the test.
#[test]
fn an_interrupted_commit_names_the_files_it_left_behind() {
    let dir = tempfile::tempdir().unwrap();
    let install = dir.path();
    let good_tmp = install.join(".tool-a.tmp.test");
    std::fs::write(&good_tmp, b"binary").unwrap();

    let pending = vec![
        Pending {
            crate_name: "tool-a".to_owned(),
            version: "1.0.0".to_owned(),
            binary_path: install.join("tool-a"),
            renames: vec![(good_tmp, install.join("tool-a"))],
        },
        Pending {
            crate_name: "tool-b".to_owned(),
            version: "2.0.0".to_owned(),
            binary_path: install.join("tool-b"),
            // Never copied, so the rename fails with ENOENT.
            renames: vec![(install.join(".tool-b.tmp.missing"), install.join("tool-b"))],
        },
    ];

    let err = commit_set(pending).expect_err("a failed commit rename must be an error");

    match &err {
        PinnedError::PlacementInterrupted {
            crate_name,
            crates_on_disk,
            placed,
            ..
        } => {
            assert_eq!(crate_name, "tool-b");
            assert_eq!(crates_on_disk, &vec!["tool-a".to_owned()]);
            assert_eq!(placed, &vec![install.join("tool-a")]);
        }
        other => panic!("expected PlacementInterrupted, got {other:?}"),
    }
    let text = err.to_string();
    assert!(
        !text.contains("nothing was installed"),
        "a partial placement must not claim a clean install dir: {text}"
    );
    assert!(
        text.contains("tool-a"),
        "the survivor must be named: {text}"
    );
    assert!(
        install.join("tool-a").exists(),
        "the test only proves anything if the first file really committed"
    );
}

/// Why: Two tools writing the same binary name would otherwise race for one
/// temporary path and surface as a confusing mid-commit ENOENT, when the real
/// problem is the set itself.
/// What: Copies a set whose two tools both ship `shared`; asserts it is rejected
/// before anything is placed, and that the install dir is left empty.
/// Test: This is the test.
#[test]
fn a_set_is_rejected_when_two_tools_claim_the_same_binary_name() {
    let src = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let staged = [
        staged_fixture("tool-a", "1.0.0", &["shared"], src.path()),
        staged_fixture("tool-b", "2.0.0", &["shared"], src.path()),
    ];

    let err = copy_set_into_install_dir(&staged, dir.path())
        .expect_err("a colliding binary name must fail closed");

    assert!(matches!(err, PinnedError::Io { .. }), "got {err:?}");
    assert!(err.to_string().contains("nothing was installed"));
    assert_nothing_installed(dir.path());
}

/// Why: The copy phase creates hidden temporaries inside the install directory;
/// if a failure left them there, "nothing was installed" would be a half-truth
/// and the next run would find litter.
/// What: Copies a valid set, then discards it, and asserts the directory is
/// empty — the same assertion every fail-closed test makes.
/// Test: This is the test.
#[test]
fn a_discarded_copy_phase_leaves_no_temporary_files() {
    let src = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let staged = [staged_fixture(
        "tool-a",
        "1.0.0",
        &["tool-a", "alias"],
        src.path(),
    )];

    let pending = copy_set_into_install_dir(&staged, dir.path()).expect("copies must succeed");
    assert_eq!(pending[0].renames.len(), 2);
    assert_nothing_installed_under_final_names(dir.path());

    discard(temporaries(&pending));
    assert_nothing_installed(dir.path());
}

/// Assert no file exists under a FINAL name — hidden temporaries are allowed.
fn assert_nothing_installed_under_final_names(install_dir: &Path) {
    let published: Vec<String> = std::fs::read_dir(install_dir)
        .map(|d| {
            d.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| !n.starts_with('.'))
                .collect()
        })
        .unwrap_or_default();
    assert!(
        published.is_empty(),
        "copy phase published {published:?} before the commit phase"
    );
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

/// Why: Both fixtures ship a `LICENSE`, exactly as every real release tarball in
/// this workspace does. Placing archive documentation made a multi-tool set
/// collide on one filename and fail, so the three-tool set `trusty-audit` needs
/// could never install (#5495). An install directory of executables also has no
/// use for a `LICENSE` it cannot tell apart from another crate's.
/// What: Installs a two-tool set; asserts the binaries land and the shared
/// documentation file does not.
/// Test: This is the test.
#[tokio::test]
async fn documentation_files_are_not_installed() {
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
    .expect("two tools shipping the same LICENSE must still install");

    assert!(
        !dir.path().join("LICENSE").exists(),
        "archive documentation must not be installed"
    );
    assert!(
        installed.iter().all(|i| i
            .paths
            .iter()
            .all(|p| p.file_name() != Some("LICENSE".as_ref()))),
        "a non-executable must not be reported as placed: {installed:?}"
    );
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

/// Preflight the set against a fixture, asking only whether each pin resolves.
async fn preflight(base: &str, tools: &[PinnedTool]) -> Vec<PinnedPreflight> {
    let releases_url = format!("{base}/releases");
    let download_base = format!("{base}/dl");
    let endpoints = Endpoints {
        releases_url: &releases_url,
        download_base: &download_base,
    };
    preflight::preflight_pinned_set_at(&crate::download::http_client(), &endpoints, tools).await
}

/// Why: #5970's whole point — a caller can ask "could this be installed" and get
/// an answer without a download.
/// What: two published pins over a fixture; both come back installable.
/// Test: This is the test.
#[tokio::test]
async fn preflight_reports_every_tool_as_installable() {
    let Some(target) = tier1() else {
        return;
    };
    let base = Fixture::new(target)
        .publish("demo-tool", "1.2.3", Flaw::None)
        .publish("other-tool", "0.4.0", Flaw::None)
        .start()
        .await;

    let checked = preflight(
        &base,
        &[
            PinnedTool::new("demo-tool", "1.2.3"),
            PinnedTool::new("other-tool", "0.4.0"),
        ],
    )
    .await;

    assert_eq!(checked.len(), 2);
    assert!(
        checked.iter().all(PinnedPreflight::installable),
        "{checked:?}"
    );
    let names: Vec<&str> = checked.iter().map(|c| c.crate_name.as_str()).collect();
    assert_eq!(names, vec!["demo-tool", "other-tool"], "order must survive");
}

/// Why: the answer has to name the TOOL and the reason, or the operator cannot
/// act on it.
/// What: one published pin, one naming a version the fixture never published;
/// the second carries `VersionNotPublished`.
/// Test: This is the test.
#[tokio::test]
async fn preflight_names_the_tool_whose_version_was_never_published() {
    let Some(target) = tier1() else {
        return;
    };
    let base = Fixture::new(target)
        .publish("demo-tool", "1.2.3", Flaw::None)
        .start()
        .await;

    let checked = preflight(
        &base,
        &[
            PinnedTool::new("demo-tool", "1.2.3"),
            PinnedTool::new("demo-tool", "9.9.9"),
        ],
    )
    .await;

    assert!(checked[0].installable(), "{checked:?}");
    assert!(
        matches!(
            checked[1].problem,
            Some(PinnedError::VersionNotPublished { .. })
        ),
        "{checked:?}"
    );
    let rendered = checked[1]
        .problem
        .as_ref()
        .expect("a problem was reported")
        .to_string();
    assert!(rendered.contains("demo-tool"), "{rendered}");
    assert!(rendered.contains("9.9.9"), "{rendered}");
}

/// Why: the set is reported WHOLE. Stopping at the first refusal would make a
/// recipient fix one tool, re-run, and meet the next.
/// What: an unpublished pin between two published ones; all three come back.
/// Test: This is the test.
#[tokio::test]
async fn preflight_reports_every_tool_even_after_one_fails() {
    let Some(target) = tier1() else {
        return;
    };
    let base = Fixture::new(target)
        .publish("demo-tool", "1.2.3", Flaw::None)
        .publish("other-tool", "0.4.0", Flaw::None)
        .start()
        .await;

    let checked = preflight(
        &base,
        &[
            PinnedTool::new("demo-tool", "1.2.3"),
            PinnedTool::new("demo-tool", "9.9.9"),
            PinnedTool::new("other-tool", "0.4.0"),
        ],
    )
    .await;

    let installable: Vec<bool> = checked.iter().map(PinnedPreflight::installable).collect();
    assert_eq!(installable, vec![true, false, true], "{checked:?}");
}

/// Why: a preflight that downloaded would be an install under another name, and
/// `install_pinned_set` must stay the only path that fetches bytes.
/// What: preflights a pin whose ARTIFACT is missing from the fixture. The pin
/// still resolves — the tag is published — which is exactly the limit: a clean
/// preflight promises resolution, never a successful download. The install over
/// the same set then fails closed, which is what makes that a limit rather than
/// a hole.
/// Test: This is the test.
#[tokio::test]
async fn preflight_downloads_nothing() {
    let Some(target) = tier1() else {
        return;
    };
    let base = Fixture::new(target)
        .publish("demo-tool", "1.2.3", Flaw::AssetMissing)
        .start()
        .await;

    let checked = preflight(&base, &[PinnedTool::new("demo-tool", "1.2.3")]).await;
    assert!(
        checked[0].installable(),
        "a published tag resolves even when its asset 404s: {checked:?}"
    );

    let dir = tempfile::tempdir().unwrap();
    let err = run(&base, &[PinnedTool::new("demo-tool", "1.2.3")], dir.path())
        .await
        .expect_err("a missing artifact must still fail the install");
    assert!(
        matches!(err, PinnedError::ArtifactUnavailable { .. }),
        "got {err:?}"
    );
    assert_nothing_installed(dir.path());
}

/// Why: an unreachable release list is not "not published", and the preflight
/// owes the same distinction the install does.
/// What: points the resolver at a port that refuses.
/// Test: This is the test.
#[tokio::test]
async fn preflight_distinguishes_an_unreachable_list_from_an_unpublished_pin() {
    if tier1().is_none() {
        return;
    }
    let base = format!("http://{}", crate::commands::test_support::dead_addr());
    let checked = preflight(&base, &[PinnedTool::new("demo-tool", "1.2.3")]).await;

    assert!(
        matches!(
            checked[0].problem,
            Some(PinnedError::ReleaseLookupFailed { .. })
        ),
        "{checked:?}"
    );
}
