//! #9974 — a prebuilt trusty-search install must take cargo's record of the
//! `trusty-embedderd-py` launcher over from a standalone install.
//!
//! Why: the prebuilt path wrote the launcher binary and left
//! `.crates.toml` / `.crates2.json` naming the standalone package as its
//! owner, so a later `cargo uninstall trusty-embedderd-py` deleted the
//! launcher trusty-search needs.
//!
//! What: each test seeds a fixture cargo root (`<root>/bin` plus the two
//! metadata files) and drives the real prebuilt entry,
//! [`super::try_install_prebuilt_at`], against a loopback release fixture.
//! Skipped on a host with no Tier-1 target, where the orchestrator falls back
//! before any request — the same guard `mod_tests` uses.
//!
//! Test: this is the test module.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::test_fixture::{fake_tarball_bins, serve_fixture, sha256_hex, Routes};
use super::*;

const CRATES_IO: &str = "registry+https://github.com/rust-lang/crates.io-index";
const SEARCH: &str = "trusty-search";
const SEARCH_VERSION: &str = "0.61.0";
const SEARCH_BINS: [&str; 3] = ["trusty-search", "trusty-embedderd", "trusty-embedderd-py"];
const STANDALONE_KEY: &str =
    "trusty-embedderd-py 0.1.5 (registry+https://github.com/rust-lang/crates.io-index)";
const RIPGREP_KEY: &str = "ripgrep 14.1.1 (registry+https://github.com/rust-lang/crates.io-index)";

/// `name` as cargo records it on this platform.
fn exe(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

/// A v2 record as `cargo install` writes it.
fn v2_info(bins: &[&str]) -> Value {
    let bins: Vec<String> = bins.iter().map(|b| exe(b)).collect();
    json!({
        "version_req": null, "bins": bins, "features": [], "all_features": false,
        "no_default_features": false, "profile": "release",
        "target": "x86_64-unknown-linux-gnu", "rustc": "rustc 1.94.0 (4a4ef493e 2026-03-02)"
    })
}

/// A fixture cargo root: `ripgrep` always, the standalone launcher record when
/// `standalone`, and the old launcher binary on disk.
fn seed_root(root: &Path, standalone: bool) -> PathBuf {
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).expect("bin dir");
    let mut v1 = format!("[v1]\n\"{RIPGREP_KEY}\" = [\"{}\"]\n", exe("rg"));
    let mut installs = serde_json::Map::new();
    installs.insert(RIPGREP_KEY.to_owned(), v2_info(&["rg"]));
    if standalone {
        v1.push_str(&format!(
            "\"{STANDALONE_KEY}\" = [\"{}\"]\n",
            exe("trusty-embedderd-py")
        ));
        installs.insert(STANDALONE_KEY.to_owned(), v2_info(&["trusty-embedderd-py"]));
        std::fs::write(bin.join(exe("trusty-embedderd-py")), "old launcher").expect("launcher");
    }
    std::fs::write(root.join(".crates.toml"), v1).expect("v1");
    let v2 = json!({ "installs": installs });
    std::fs::write(root.join(".crates2.json"), v2.to_string()).expect("v2");
    bin
}

/// Install a three-binary trusty-search prebuilt into `bin_dir` through the
/// loopback release fixture. `None` on a host with no Tier-1 target.
async fn install_search(bin_dir: &Path) -> Option<Outcome> {
    let target = platform::current_target()?;
    let suffix = glibc::select_asset_suffix(SEARCH, target, glibc::host_glibc_version()).suffix;
    let archive = release::asset_filename(SEARCH, SEARCH_VERSION, &suffix);
    let tarball = fake_tarball_bins(&SEARCH_BINS, &format!("{SEARCH} {SEARCH_VERSION}"));
    let digest = sha256_hex(&tarball);
    let mut routes: Routes = std::collections::HashMap::new();
    routes.insert(
        "/releases".to_owned(),
        (
            200,
            format!(r#"[{{"tag_name":"{SEARCH}-v{SEARCH_VERSION}","prerelease":false}}]"#)
                .into_bytes(),
        ),
    );
    let key = format!("/dl/{SEARCH}-v{SEARCH_VERSION}/{archive}");
    routes.insert(
        format!("{key}.sha256"),
        (200, format!("{digest}  {archive}\n").into_bytes()),
    );
    routes.insert(key, (200, tarball));
    let base = serve_fixture(routes).await;
    let (releases_url, download_base) = (format!("{base}/releases"), format!("{base}/dl"));
    let endpoints = pinned::Endpoints {
        releases_url: &releases_url,
        download_base: &download_base,
    };
    Some(try_install_prebuilt_at(&http_client(), &endpoints, SEARCH, bin_dir, None).await)
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).expect("metadata readable")
}

/// v1 lines that open a record for package `name`.
fn v1_records<'a>(v1: &'a str, name: &str) -> Vec<&'a str> {
    let open = format!("\"{name} ");
    v1.lines()
        .filter(|l| l.trim_start().starts_with(&open))
        .collect()
}

/// Why: THE regression (#9974). The prebuilt path placed the launcher and left
/// both metadata files naming the standalone package as its owner.
/// What: seeds the standalone record beside an unrelated `ripgrep` record,
/// installs the prebuilt, and asserts: no standalone record in either file;
/// one trusty-search record owning all three binaries in each; `ripgrep`
/// unchanged.
/// Test: this is the test.
#[tokio::test]
async fn prebuilt_install_takes_the_standalone_launcher_record_over() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("cargo");
    let bin = seed_root(&root, true);
    let Some(outcome) = install_search(&bin).await else {
        return;
    };
    assert!(
        matches!(outcome, Outcome::Installed { .. }),
        "the prebuilt installs: {outcome:?}"
    );

    let v2: Value = serde_json::from_str(&read(&root.join(".crates2.json"))).expect("v2 json");
    let installs = v2["installs"].as_object().expect("installs object");
    let standalone: Vec<&String> = installs
        .keys()
        .filter(|k| k.starts_with("trusty-embedderd-py "))
        .collect();
    assert!(
        standalone.is_empty(),
        ".crates2.json still names the standalone package: {standalone:?}"
    );
    let search_key = format!("{SEARCH} {SEARCH_VERSION} ({CRATES_IO})");
    let mut want: Vec<String> = SEARCH_BINS.iter().map(|b| exe(b)).collect();
    want.sort();
    assert_eq!(
        installs.get(&search_key).map(|i| i["bins"].clone()),
        Some(json!(want)),
        "trusty-search must own all three binaries in .crates2.json: {v2}"
    );
    assert_eq!(installs.get(RIPGREP_KEY), Some(&v2_info(&["rg"])));

    let v1 = read(&root.join(".crates.toml"));
    assert!(
        v1_records(&v1, "trusty-embedderd-py").is_empty(),
        ".crates.toml still names the standalone package:\n{v1}"
    );
    let search = v1_records(&v1, SEARCH);
    assert_eq!(search.len(), 1, "one trusty-search record:\n{v1}");
    assert!(search[0].contains(&search_key), "{v1}");
    for bin in &want {
        assert!(search[0].contains(&format!("\"{bin}\"")), "{bin}: {v1}");
    }
    assert!(
        v1.contains(&format!("\"{RIPGREP_KEY}\" = [\"{}\"]\n", exe("rg"))),
        "ripgrep's record must not change:\n{v1}"
    );
}

/// Why: fail-open check (#9974). Metadata the takeover cannot parse must never
/// be rewritten blind, and the install must not report a plain success while
/// the standalone record survives.
/// What: `.crates.toml` holds the standalone record and `.crates2.json` is not
/// JSON. Asserts the outcome is not `Installed`, that it names
/// `.crates2.json`, and that both files are byte-for-byte unchanged.
/// Test: this is the test.
#[tokio::test]
async fn malformed_metadata_is_reported_and_left_unchanged() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("cargo");
    let bin = seed_root(&root, true);
    std::fs::write(root.join(".crates2.json"), "{\"installs\": {not json").expect("v2");
    let before = (
        read(&root.join(".crates.toml")),
        read(&root.join(".crates2.json")),
    );
    let Some(outcome) = install_search(&bin).await else {
        return;
    };
    let text = format!("{outcome:?}");
    assert!(
        !matches!(outcome, Outcome::Installed { .. }),
        "a takeover that could not run must not be a plain success: {text}"
    );
    assert!(text.contains(".crates2.json"), "must name the file: {text}");
    let after = (
        read(&root.join(".crates.toml")),
        read(&root.join(".crates2.json")),
    );
    assert_eq!(before, after, "malformed metadata must be left as it was");
}

/// Why: the takeover must touch nothing on a host that never installed the
/// standalone package (#9974 ruling, item 2).
/// What: seeds only `ripgrep`; asserts both files are byte-for-byte unchanged.
/// Test: this is the test.
#[tokio::test]
async fn a_root_without_the_standalone_record_is_untouched() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("cargo");
    let bin = seed_root(&root, false);
    let before = (
        read(&root.join(".crates.toml")),
        read(&root.join(".crates2.json")),
    );
    let Some(outcome) = install_search(&bin).await else {
        return;
    };
    assert!(matches!(outcome, Outcome::Installed { .. }), "{outcome:?}");
    let after = (
        read(&root.join(".crates.toml")),
        read(&root.join(".crates2.json")),
    );
    assert_eq!(before, after);
}

/// Why: a root with no cargo metadata at all must stay that way (#9974).
/// What: installs into a bare `bin` dir; asserts no metadata file appears.
/// Test: this is the test.
#[tokio::test]
async fn a_root_without_metadata_files_gets_none() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("cargo");
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).expect("bin dir");
    let Some(outcome) = install_search(&bin).await else {
        return;
    };
    assert!(matches!(outcome, Outcome::Installed { .. }), "{outcome:?}");
    for file in [".crates.toml", ".crates2.json"] {
        assert!(!root.join(file).exists(), "{file} must not be created");
    }
}

/// Why: the fail-open rule (#9974) asks for an error that names the file and
/// the manual fix, and warns off the command that would delete the launcher.
/// What: renders a [`CargoRecordError`] and checks each part.
/// Test: this is the test.
#[test]
fn error_text_names_the_file_and_the_manual_fix() {
    let e = CargoRecordError {
        crate_name: SEARCH.to_owned(),
        version: SEARCH_VERSION.to_owned(),
        file: PathBuf::from("/home/u/.cargo/.crates2.json"),
        reason: "not JSON: expected value".to_owned(),
    };
    let text = e.to_string();
    for needle in [
        "/home/u/.cargo/.crates2.json",
        "not JSON: expected value",
        "tctl install trusty-search",
        "cargo install trusty-search --locked --force",
        "Do not run `cargo uninstall trusty-embedderd-py`",
    ] {
        assert!(text.contains(needle), "{needle:?} missing: {text}");
    }
}

/// Why: "no other package's record changes" (#9974) — a binary a third
/// package owns stays that package's, and its record is not rewritten.
/// What: `ripgrep` is seeded as owner of `trusty-embedderd`; after the
/// takeover it still is, and trusty-search claims only the other two.
/// Test: this is the test.
#[tokio::test]
async fn a_binary_a_third_package_owns_is_not_claimed() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("cargo");
    let bin = seed_root(&root, true);
    let v1 = read(&root.join(".crates.toml")).replace(
        &format!("= [\"{}\"]\n\"{STANDALONE_KEY}", exe("rg")),
        &format!(
            "= [\"{}\", \"{}\"]\n\"{STANDALONE_KEY}",
            exe("rg"),
            exe("trusty-embedderd")
        ),
    );
    std::fs::write(root.join(".crates.toml"), &v1).expect("v1");
    std::fs::remove_file(root.join(".crates2.json")).expect("v2");
    let Some(outcome) = install_search(&bin).await else {
        return;
    };
    assert!(matches!(outcome, Outcome::Installed { .. }), "{outcome:?}");
    let after = read(&root.join(".crates.toml"));
    let ripgrep_line = format!(
        "\"{RIPGREP_KEY}\" = [\"{}\", \"{}\"]\n",
        exe("rg"),
        exe("trusty-embedderd")
    );
    assert!(after.contains(&ripgrep_line), "{after}");
    let search = v1_records(&after, SEARCH);
    assert_eq!(search.len(), 1, "{after}");
    assert!(
        !search[0].contains(&format!("\"{}\"", exe("trusty-embedderd"))),
        "{after}"
    );
    assert!(
        !root.join(".crates2.json").exists(),
        "a missing file stays missing"
    );
}

/// Why: proves on real cargo what the fixture tests assume (#9974 AC): after
/// the takeover cargo still parses its metadata, `cargo uninstall
/// trusty-embedderd-py` leaves the launcher alone, and installing trusty-search
/// over the prebuilt does not fail on a bin collision.
/// What: real cargo seeds an offline path fixture of the standalone package;
/// the prebuilt fixture install runs into the same root; then real cargo runs
/// `install --list`, `uninstall trusty-embedderd-py`, and `install --path` of a
/// three-binary trusty-search fixture.
/// Test: this is the test.
#[tokio::test]
#[ignore = "runs real cargo install on offline path fixtures; run with --include-ignored"]
async fn real_cargo_accepts_the_takeover() {
    use crate::commands::standalone_launcher::tests::{real_cargo, write_fixture};
    let tmp = tempfile::tempdir().expect("tempdir");
    let (old, new) = (tmp.path().join("old"), tmp.path().join("new"));
    let (root, target) = (tmp.path().join("root"), tmp.path().join("target"));
    write_fixture(
        &old,
        "trusty-embedderd-py",
        "0.1.5",
        &["trusty-embedderd-py"],
    );
    write_fixture(&new, SEARCH, SEARCH_VERSION, &SEARCH_BINS);
    let (root_s, old_s, new_s) = (
        root.to_str().expect("utf8"),
        old.to_str().expect("utf8"),
        new.to_str().expect("utf8"),
    );
    let (ok, log) = real_cargo(
        &["install", "--offline", "--path", old_s, "--root", root_s],
        &target,
    );
    assert!(ok, "seed install failed: {log}");

    let Some(outcome) = install_search(&root.join("bin")).await else {
        return;
    };
    assert!(matches!(outcome, Outcome::Installed { .. }), "{outcome:?}");

    let (ok, listing) = real_cargo(&["install", "--list", "--root", root_s], &target);
    assert!(ok, "cargo must parse the rewritten metadata: {listing}");
    assert!(!listing.contains("trusty-embedderd-py v0.1.5"), "{listing}");
    assert!(listing.contains("trusty-search v0.61.0"), "{listing}");

    let launcher = root.join("bin").join(exe("trusty-embedderd-py"));
    let (ok, log) = real_cargo(
        &["uninstall", "trusty-embedderd-py", "--root", root_s],
        &target,
    );
    assert!(
        !ok,
        "no package may own the launcher but trusty-search: {log}"
    );
    assert!(
        launcher.exists(),
        "uninstall must leave the launcher: {log}"
    );

    let (ok, log) = real_cargo(
        &["install", "--offline", "--path", new_s, "--root", root_s],
        &target,
    );
    assert!(
        ok,
        "installing trusty-search over its prebuilt must not collide: {log}"
    );
}
