//! Route-decision table for `tm reinstall --binary` (see `binary_reinstall.rs`).
//!
//! Why: this command replaces the executable the operator is running. Every
//! ambiguity must refuse, and "refuses" has to be pinned per branch — a route
//! that silently degrades to "reinstall from crates.io" on an unclassifiable
//! install is the #4033 incident with an automation attached.
//! What: one test per branch of [`super::reinstall_route`], each asserting the
//! variant and (for a refusal) that the reason names the actual condition.
//! Test: this file is the test.

use super::*;
use crate::core::binary_provenance::{CargoInstall, InstallSource};

/// A ledger record providing `tm`, with the given version and source.
fn record(version: &str, source: InstallSource) -> CargoInstall {
    CargoInstall {
        package: "trusty-mpm".to_string(),
        version: version.to_string(),
        source,
        bins: vec!["tm".to_string(), "trusty-mpm".to_string()],
    }
}

/// Assert a refusal and return its reason.
fn reason(route: ReinstallRoute) -> String {
    match route {
        ReinstallRoute::Refuse(why) => why,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// A `same_file`-passing pair: the running exe IS `<cargo_bin>/tm`.
struct Host {
    tmp: tempfile::TempDir,
    exe: std::path::PathBuf,
    bin_dir: std::path::PathBuf,
}

fn host() -> Host {
    let tmp = tempfile::TempDir::new().unwrap();
    let bin_dir = tmp.path().join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let exe = bin_dir.join("tm");
    std::fs::write(&exe, "binary").unwrap();
    Host { tmp, exe, bin_dir }
}

#[test]
fn route_refuses_without_a_ledger() {
    // No ledger means provenance was never determined. Guessing a route here
    // is the whole failure class this command must not join.
    let h = host();
    let why = reason(reinstall_route(
        "tm",
        "1.3.5",
        &h.exe,
        &h.bin_dir,
        None,
        &|_| true,
    ));
    assert!(why.contains(".crates2.json"), "{why}");
}

#[test]
fn route_refuses_when_not_in_ledger() {
    // A prebuilt-installer or package-manager drop: there is no cargo command
    // to repeat, so there is nothing to run.
    let h = host();
    let ledger = [CargoInstall {
        package: "other".to_string(),
        version: "1.0.0".to_string(),
        source: InstallSource::Registry,
        bins: vec!["other".to_string()],
    }];
    let why = reason(reinstall_route(
        "tm",
        "1.3.5",
        &h.exe,
        &h.bin_dir,
        Some(&ledger),
        &|_| true,
    ));
    assert!(
        why.contains("not recorded in cargo's install ledger"),
        "{why}"
    );
}

#[test]
fn route_refuses_on_duplicate_installs() {
    // The #4033 trusty-channels case: two installs provide the same binary, so
    // reinstalling one does not determine what runs.
    let h = host();
    let ledger = [
        record("1.3.5", InstallSource::Registry),
        record("1.3.4", InstallSource::Path("/tmp/somewhere".into())),
    ];
    let why = reason(reinstall_route(
        "tm",
        "1.3.5",
        &h.exe,
        &h.bin_dir,
        Some(&ledger),
        &|_| true,
    ));
    assert!(why.contains("2 separate installs"), "{why}");
}

#[test]
fn route_refuses_when_running_binary_is_not_the_cargo_install() {
    // `which -a tm` returning two different files is a measured state on the
    // #4033 host. Reinstalling cargo's copy would leave the shadowing one
    // running, so the command must say that rather than appear to succeed.
    let h = host();
    let elsewhere = h.tmp.path().join("local-bin");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let shadow = elsewhere.join("tm");
    std::fs::write(&shadow, "a different binary").unwrap();

    let ledger = [record("1.3.5", InstallSource::Registry)];
    let why = reason(reinstall_route(
        "tm",
        "1.3.5",
        &shadow,
        &h.bin_dir,
        Some(&ledger),
        &|_| true,
    ));
    assert!(why.contains("DIFFERENT file"), "{why}");
}

#[test]
fn route_refuses_on_version_disagreement() {
    let h = host();
    let ledger = [record("1.3.4", InstallSource::Registry)];
    let why = reason(reinstall_route(
        "tm",
        "1.3.5",
        &h.exe,
        &h.bin_dir,
        Some(&ledger),
        &|_| true,
    ));
    assert!(why.contains("1.3.4"), "{why}");
}

#[test]
fn route_refuses_when_path_source_is_gone() {
    // A `cargo install --path` source under a directory macOS reaped: no
    // provenance, nothing to rebuild from, so the refusal names the crates.io
    // fallback instead of silently taking it.
    let h = host();
    let ledger = [record(
        "1.3.5",
        InstallSource::Path("/Users/x/tmp/wt".into()),
    )];
    let why = reason(reinstall_route(
        "tm",
        "1.3.5",
        &h.exe,
        &h.bin_dir,
        Some(&ledger),
        &|_| false,
    ));
    assert!(why.contains("NO LONGER EXISTS"), "{why}");
    assert!(why.contains("cargo install trusty-mpm --locked"), "{why}");
}

#[test]
fn route_refuses_for_a_git_install() {
    let h = host();
    let ledger = [record("1.3.5", InstallSource::Git("https://x#abc".into()))];
    let why = reason(reinstall_route(
        "tm",
        "1.3.5",
        &h.exe,
        &h.bin_dir,
        Some(&ledger),
        &|_| true,
    ));
    assert!(why.contains("git"), "{why}");
}

#[test]
fn route_refuses_for_an_unrecognised_source() {
    let h = host();
    let ledger = [record("1.3.5", InstallSource::Other("sparse+weird".into()))];
    let why = reason(reinstall_route(
        "tm",
        "1.3.5",
        &h.exe,
        &h.bin_dir,
        Some(&ledger),
        &|_| true,
    ));
    assert!(why.contains("unrecognised source"), "{why}");
}

#[test]
fn route_reinstalls_a_live_path_install() {
    let h = host();
    let ledger = [record(
        "1.3.5",
        InstallSource::Path("/src/trusty-mpm".into()),
    )];
    let route = reinstall_route("tm", "1.3.5", &h.exe, &h.bin_dir, Some(&ledger), &|_| true);
    assert_eq!(
        route,
        ReinstallRoute::Path {
            package: "trusty-mpm".to_string(),
            dir: "/src/trusty-mpm".into(),
        }
    );
}

#[test]
fn route_upgrades_a_registry_install() {
    let h = host();
    let ledger = [record("1.3.5", InstallSource::Registry)];
    let route = reinstall_route("tm", "1.3.5", &h.exe, &h.bin_dir, Some(&ledger), &|_| true);
    assert_eq!(
        route,
        ReinstallRoute::Registry {
            package: "trusty-mpm".to_string(),
        }
    );
}
