//! #10584 — `tctl install` must never replace a newer installed build.
//!
//! Why: each fail-closed arm of the guard is a separate way a dev build could
//! be silently downgraded, so each has its own test.
//! What: pure comparison tests, `--version` probes against stand-in scripts,
//! and the cargo-fallback lookup against a loopback crates.io fixture.
//! Test: This is the test module.

use super::*;
use crate::download::test_fixture::{serve_fixture, Routes};

/// A generous per-request bound for the fixture-backed lookups.
const LIMIT: Duration = Duration::from_secs(10);

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
    let err = guard_cargo_fallback_at(&client, &api, "demo-tool", Some(&build("2.0.0")), LIMIT)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DowngradeRefusal::NewerInstalled { .. }),
        "{err:?}"
    );
    // A newer crates.io release, or nothing installed, proceeds.
    let api = crates_io_fixture(200, r#"{"crate":{"max_stable_version":"2.1.0"}}"#).await;
    assert!(
        guard_cargo_fallback_at(&client, &api, "demo-tool", Some(&build("2.0.0")), LIMIT)
            .await
            .is_ok()
    );
    assert!(
        guard_cargo_fallback_at(&client, &api, "demo-tool", None, LIMIT)
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn cargo_fallback_refuses_when_crates_io_is_unreachable() {
    let api = crates_io_fixture(500, "boom").await;
    let client = crate::download::http_client();
    let err = guard_cargo_fallback_at(&client, &api, "demo-tool", Some(&build("2.0.0")), LIMIT)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DowngradeRefusal::UnknownTarget { .. }),
        "{err:?}"
    );
}

/// Why (#10584 review, MEDIUM): a stalled crates.io connection must end in a
/// refusal, not a hang.
/// What: a loopback listener accepts and never answers; with a 300 ms bound the
/// guard returns `UnknownTarget`.
#[tokio::test]
async fn cargo_fallback_refuses_when_crates_io_stalls() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((sock, _)) = listener.accept().await {
            held.push(sock);
        }
    });
    let client = crate::download::http_client();
    let api = format!("http://{addr}/api/v1/crates");
    let limit = Duration::from_millis(300);
    let installed = build("2.0.0");
    let lookup = guard_cargo_fallback_at(&client, &api, "demo-tool", Some(&installed), limit);
    let err = tokio::time::timeout(Duration::from_secs(10), lookup)
        .await
        .expect("the lookup's own timeout must fire first")
        .unwrap_err();
    assert!(
        matches!(err, DowngradeRefusal::UnknownTarget { .. }),
        "{err:?}"
    );
}

/// Why (#10584 review, HIGH): a `--version` that hangs must not leave its
/// process running after the probe gives up.
/// What: the stand-in records its pid and `exec`s a long sleep; after the
/// probe's timeout the pid must be gone (`kill -0` fails). The 3 s bound
/// leaves a freshly written script time to start (and record its pid) under
/// full-suite load; 300 ms was too short there.
#[cfg(unix)]
#[tokio::test]
async fn a_hung_probe_is_killed_at_the_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("pid");
    let body = format!("echo $$ > '{}'\nexec sleep 30", pid_file.display());
    let path = crate::download::test_fixture::write_fake_binary(dir.path(), "demo-tool", &body);
    let err = probe_installed_build_within("demo-tool", &[&path], Duration::from_secs(3))
        .await
        .unwrap_err();
    assert!(
        matches!(err, DowngradeRefusal::UnreadableInstalled { .. }),
        "{err:?}"
    );
    let pid = std::fs::read_to_string(&pid_file).expect("the stand-in never started");
    let alive = std::process::Command::new("kill")
        .args(["-0", pid.trim()])
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success();
    if alive {
        // Clean up before failing so the sleep does not outlive the test.
        let _ = std::process::Command::new("kill")
            .args(["-9", pid.trim()])
            .status();
    }
    assert!(
        !alive,
        "the hung `--version` child {} outlived the probe",
        pid.trim()
    );
}
