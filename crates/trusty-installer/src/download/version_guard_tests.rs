//! #10584 — `tctl install` must never replace a newer installed build.
//!
//! Why: each fail-closed arm of the guard is a separate way a dev build could
//! be silently downgraded, so each has its own test.
//! What: pure comparison tests, `--version` probes against stand-in scripts,
//! and the cargo-fallback lookup against a loopback crates.io fixture.
//! Test: This is the test module.

use super::*;
use crate::download::test_fixture::{serve_fixture, Routes};

fn build(version: &str) -> InstalledBuild {
    InstalledBuild {
        path: PathBuf::from("/bin/demo-tool"),
        version: version.to_owned(),
    }
}

#[test]
fn newer_installed_build_is_refused_naming_both_versions() {
    let err = check_no_downgrade("demo-tool", &build("2.0.0-dev"), "1.9.0").unwrap_err();
    assert!(
        matches!(err, DowngradeRefusal::NewerInstalled { .. }),
        "{err:?}"
    );
    let msg = err.to_string();
    assert!(msg.contains("2.0.0-dev") && msg.contains("1.9.0"), "{msg}");
    assert!(msg.contains("/bin/demo-tool"), "{msg}");
}

#[test]
fn equal_or_older_installed_build_proceeds() {
    for (have, target) in [
        ("1.9.0", "1.9.0"),
        ("1.8.0", "1.9.0"),
        // A pre-release of the target is older than the release.
        ("1.9.0-dev", "1.9.0"),
        // Build metadata never orders two builds.
        ("1.9.0+abc123", "1.9.0"),
    ] {
        assert!(
            check_no_downgrade("demo-tool", &build(have), target).is_ok(),
            "{have} -> {target}"
        );
    }
}

#[test]
fn unparseable_versions_are_refused() {
    let err = check_no_downgrade("demo-tool", &build("dev"), "1.0.0").unwrap_err();
    assert!(
        matches!(err, DowngradeRefusal::UnreadableInstalled { .. }),
        "{err:?}"
    );
    let err = check_no_downgrade("demo-tool", &build("1.0.0"), "latest").unwrap_err();
    assert!(
        matches!(err, DowngradeRefusal::UnknownTarget { .. }),
        "{err:?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn probe_reads_the_installed_version() {
    let dir = tempfile::tempdir().unwrap();
    let path = crate::download::test_fixture::write_fake_binary(
        dir.path(),
        "demo-tool",
        "echo 'demo-tool 2.1.0'",
    );
    // The same path twice (preferred == cargo bin dir) probes once.
    let got = probe_installed_build("demo-tool", &[&path, &path])
        .await
        .unwrap();
    assert_eq!(
        got,
        Some(InstalledBuild {
            path,
            version: "2.1.0".to_owned()
        })
    );
}

#[tokio::test]
async fn probe_is_none_when_nothing_is_installed() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("demo-tool");
    let got = probe_installed_build("demo-tool", &[&missing])
        .await
        .unwrap();
    assert_eq!(got, None);
}

/// The failure branch: a binary that exists but cannot report its version is
/// refused, never treated as "nothing installed" and overwritten.
#[cfg(unix)]
#[tokio::test]
async fn an_unreadable_installed_binary_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    for (name, body) in [
        ("exits-nonzero", "exit 1"),
        ("no-version", "echo 'hello'"),
        ("not-semver", "echo 'demo-tool 2.0'"),
    ] {
        let path = crate::download::test_fixture::write_fake_binary(dir.path(), name, body);
        let err = probe_installed_build("demo-tool", &[&path])
            .await
            .unwrap_err();
        assert!(
            matches!(err, DowngradeRefusal::UnreadableInstalled { .. }),
            "{name}: {err:?}"
        );
        assert!(err.to_string().contains("nothing was replaced"), "{err}");
    }
}

async fn crates_io_fixture(status: u16, body: &str) -> String {
    let mut routes: Routes = std::collections::HashMap::new();
    routes.insert(
        "/api/v1/crates/demo-tool".to_owned(),
        (status, body.as_bytes().to_vec()),
    );
    format!("{}/api/v1/crates", serve_fixture(routes).await)
}

#[tokio::test]
async fn cargo_fallback_refuses_an_older_crates_io_release() {
    let api = crates_io_fixture(200, r#"{"crate":{"max_stable_version":"1.0.0"}}"#).await;
    let client = crate::download::http_client();
    let err = guard_cargo_fallback_at(&client, &api, "demo-tool", Some(&build("2.0.0")))
        .await
        .unwrap_err();
    assert!(
        matches!(err, DowngradeRefusal::NewerInstalled { .. }),
        "{err:?}"
    );
    // A newer crates.io release, or nothing installed, proceeds.
    let api = crates_io_fixture(200, r#"{"crate":{"max_stable_version":"2.1.0"}}"#).await;
    assert!(
        guard_cargo_fallback_at(&client, &api, "demo-tool", Some(&build("2.0.0")))
            .await
            .is_ok()
    );
    assert!(guard_cargo_fallback_at(&client, &api, "demo-tool", None)
        .await
        .is_ok());
}

#[tokio::test]
async fn cargo_fallback_refuses_when_crates_io_is_unreachable() {
    let api = crates_io_fixture(500, "boom").await;
    let client = crate::download::http_client();
    let err = guard_cargo_fallback_at(&client, &api, "demo-tool", Some(&build("2.0.0")))
        .await
        .unwrap_err();
    assert!(
        matches!(err, DowngradeRefusal::UnknownTarget { .. }),
        "{err:?}"
    );
}
