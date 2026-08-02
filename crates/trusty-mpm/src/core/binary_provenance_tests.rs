//! Tests for [`super`] — cargo install-ledger parsing and the `#4033`
//! provenance verdict table.
//!
//! Why: every branch of [`super::provenance_report`] is a claim `tm doctor`
//! makes to an operator about whether the binary they are testing is the one
//! they think it is; each therefore needs its own mutation-proof case,
//! including the two that must report `Unknown` rather than `Ok`.
//! What: source-classification cases, ledger-parse cases, then one case per
//! verdict branch.
//! Test: this file.

use super::*;

/// A `.crates2.json` document mirroring the real one's shape.
const LEDGER: &str = r#"{
  "installs": {
    "trusty-mpm 1.3.1 (registry+https://github.com/rust-lang/crates.io-index)": {
      "version_req": null,
      "bins": ["tm"],
      "features": [],
      "all_features": false,
      "no_default_features": false,
      "profile": "release",
      "target": "aarch64-apple-darwin",
      "rustc": "rustc 1.90.0",
      "supports_options": true
    },
    "trusty-search 0.39.0 (path+file:///Users/masa/tmp/ts/crates/trusty-search)": {
      "bins": ["trusty-search"]
    },
    "trusty-channels 0.1.0 (git+ssh://git@github.com/bobmatnyc/trusty-tools.git#b45f2ad)": {
      "bins": ["slack-mcp", "telegram-mcp"]
    }
  }
}"#;

fn always_exists(_: &Path) -> bool {
    true
}

/// The cargo bin directory the fixtures pretend to install into.
const CARGO_BIN: &str = "/Users/masa/.cargo/bin";

/// Path of a binary that IS the cargo install.
fn installed(bin: &str) -> PathBuf {
    PathBuf::from(CARGO_BIN).join(bin)
}

fn never_exists(_: &Path) -> bool {
    false
}

#[test]
fn parses_a_real_ledger() {
    // The three source shapes cargo actually writes must all round-trip into
    // typed records keyed by the BINARY names doctor asks about.
    let installs = parse_cargo_installs(LEDGER).expect("ledger should parse");
    assert_eq!(installs.len(), 3);

    let mpm = installs.iter().find(|i| i.package == "trusty-mpm").unwrap();
    assert_eq!(mpm.version, "1.3.1");
    assert_eq!(mpm.source, InstallSource::Registry);
    assert_eq!(mpm.bins, vec!["tm".to_string()]);

    let channels = installs
        .iter()
        .find(|i| i.package == "trusty-channels")
        .unwrap();
    assert_eq!(channels.bins.len(), 2);
}

#[test]
fn parse_returns_none_for_garbage() {
    // An unparseable ledger must be None — the caller reports UNKNOWN. An
    // empty Vec would read as "cargo installed nothing", which is a claim.
    assert!(parse_cargo_installs("not json").is_none());
}

#[test]
fn parse_skips_malformed_keys() {
    // A key that is not a package-id spec is skipped, never guessed at.
    let installs = parse_cargo_installs(r#"{"installs":{"nonsense":{"bins":["x"]}}}"#).unwrap();
    assert!(installs.is_empty());
}

#[test]
fn classifies_registry_source() {
    assert_eq!(
        classify_source("registry+https://github.com/rust-lang/crates.io-index"),
        InstallSource::Registry
    );
}

#[test]
fn classifies_path_source() {
    assert_eq!(
        classify_source("path+file:///Users/masa/tmp/x"),
        InstallSource::Path(PathBuf::from("/Users/masa/tmp/x"))
    );
}

#[test]
fn classifies_git_source() {
    assert!(matches!(
        classify_source("git+ssh://git@example.com/x.git#abc"),
        InstallSource::Git(_)
    ));
}

#[test]
fn classifies_unknown_source() {
    // An unrecognised source is carried as-is, never silently normalised into
    // one of the known kinds.
    assert!(matches!(
        classify_source("sparse+https://example.invalid"),
        InstallSource::Other(_)
    ));
}

#[test]
fn unknown_without_a_ledger() {
    // No ledger = nothing learned. UNKNOWN, never Ok (the whole point of #4033).
    let (status, msg) = provenance_report(
        "tm",
        "1.3.1",
        &installed("tm"),
        Path::new(CARGO_BIN),
        None,
        &always_exists,
    );
    assert_eq!(status, CheckStatus::Unknown, "message: {msg}");
    assert!(msg.contains("cannot verify"), "message: {msg}");
}

#[test]
fn unknown_when_bin_is_not_in_the_ledger() {
    // A prebuilt-installer drop into ~/.local/bin is real and common; doctor
    // must say it cannot verify it rather than call it clean.
    let ledger = parse_cargo_installs(LEDGER).unwrap();
    let (status, msg) = provenance_report(
        "tctl",
        "0.4.3",
        Path::new("/Users/masa/.local/bin/tctl"),
        Path::new(CARGO_BIN),
        Some(&ledger),
        &always_exists,
    );
    assert_eq!(status, CheckStatus::Unknown, "message: {msg}");
    assert!(msg.contains("not recorded"), "message: {msg}");
}

#[test]
fn fails_on_duplicate_installs() {
    // The #4033 trusty-channels case: the same binary from two different
    // sources, whichever wrote last winning.
    let ledger = vec![
        CargoInstall {
            package: "trusty-channels".into(),
            version: "0.1.0".into(),
            source: InstallSource::Path(PathBuf::from("/a")),
            bins: vec!["slack-mcp".into()],
        },
        CargoInstall {
            package: "trusty-channels".into(),
            version: "0.1.1".into(),
            source: InstallSource::Path(PathBuf::from("/b")),
            bins: vec!["slack-mcp".into()],
        },
    ];
    let (status, msg) = provenance_report(
        "slack-mcp",
        "0.1.0",
        &installed("slack-mcp"),
        Path::new(CARGO_BIN),
        Some(&ledger),
        &always_exists,
    );
    assert_eq!(status, CheckStatus::Fail, "message: {msg}");
    assert!(msg.contains("2 separate installs"), "message: {msg}");
}

#[test]
fn fails_on_version_disagreement() {
    // The running binary is not the one cargo tracks — an upgrade would not
    // replace what is actually running.
    let ledger = parse_cargo_installs(LEDGER).unwrap();
    let (status, msg) = provenance_report(
        "tm",
        "1.2.3",
        &installed("tm"),
        Path::new(CARGO_BIN),
        Some(&ledger),
        &always_exists,
    );
    assert_eq!(status, CheckStatus::Fail, "message: {msg}");
    assert!(msg.contains("1.2.3"), "message: {msg}");
    assert!(msg.contains("1.3.1"), "message: {msg}");
}

#[test]
fn fails_when_path_source_is_gone() {
    // The `/Users/masa/tmp/` case: macOS reaped the source, so there is no
    // provenance AND no way to rebuild.
    let ledger = parse_cargo_installs(LEDGER).unwrap();
    let (status, msg) = provenance_report(
        "trusty-search",
        "0.39.0",
        &installed("trusty-search"),
        Path::new(CARGO_BIN),
        Some(&ledger),
        &never_exists,
    );
    assert_eq!(status, CheckStatus::Fail, "message: {msg}");
    assert!(msg.contains("NO LONGER EXISTS"), "message: {msg}");
}

#[test]
fn warns_for_a_live_path_install() {
    // Source still there: it works, but it bypasses update detection.
    let ledger = parse_cargo_installs(LEDGER).unwrap();
    let (status, msg) = provenance_report(
        "trusty-search",
        "0.39.0",
        &installed("trusty-search"),
        Path::new(CARGO_BIN),
        Some(&ledger),
        &always_exists,
    );
    assert_eq!(status, CheckStatus::Warn, "message: {msg}");
    assert!(
        msg.contains("invisible to registry update"),
        "message: {msg}"
    );
}

#[test]
fn warns_for_a_git_install() {
    let ledger = parse_cargo_installs(LEDGER).unwrap();
    let (status, msg) = provenance_report(
        "slack-mcp",
        "0.1.0",
        &installed("slack-mcp"),
        Path::new(CARGO_BIN),
        Some(&ledger),
        &always_exists,
    );
    assert_eq!(status, CheckStatus::Warn, "message: {msg}");
    assert!(msg.contains("git"), "message: {msg}");
}

#[test]
fn ok_for_a_matching_registry_install() {
    // The only clean state: registry source, versions agree.
    let ledger = parse_cargo_installs(LEDGER).unwrap();
    let (status, msg) = provenance_report(
        "tm",
        "1.3.1",
        &installed("tm"),
        Path::new(CARGO_BIN),
        Some(&ledger),
        &always_exists,
    );
    assert_eq!(status, CheckStatus::Ok, "message: {msg}");
    assert!(msg.contains("crates.io"), "message: {msg}");
}

/// #4622 review HIGH-2: a foreign binary must NOT inherit cargo's provenance.
///
/// Why: attribution was by BASENAME alone — `running_exe` only ever went into
/// the message string. On the #4033 host `which -a tm` returns
/// `~/.cargo/bin/tm` (1.3.1, the cargo install) and `~/.local/bin/tm` (0.19.29,
/// a prebuilt-installer drop). This test pins the harder half of that scenario:
/// the foreign binary's version COINCIDES with the ledger's, so the version
/// comparison cannot save the check. Before the fix it reported
/// `Ok — installed from crates.io; cargo's ledger agrees`.
/// Test: this test.
#[test]
fn unknown_when_running_binary_is_not_the_cargo_install() {
    let ledger = parse_cargo_installs(LEDGER).unwrap();
    // Same basename, same version — only the PATH differs.
    let (status, msg) = provenance_report(
        "tm",
        "1.3.1",
        Path::new("/Users/masa/.local/bin/tm"),
        Path::new(CARGO_BIN),
        Some(&ledger),
        &always_exists,
    );

    assert_eq!(
        status,
        CheckStatus::Unknown,
        "a binary cargo did not install must never inherit the cargo record: {msg}"
    );
    assert!(msg.contains("/Users/masa/.local/bin/tm"), "message: {msg}");
    assert!(msg.contains("/Users/masa/.cargo/bin/tm"), "message: {msg}");
    assert!(
        msg.contains("which -a"),
        "message must name the diagnosis: {msg}"
    );
}

/// The control for the proof above: identical inputs except the running path,
/// which now IS the cargo install — the same ledger then reports Ok.
#[test]
fn ok_when_running_binary_is_the_cargo_install() {
    let ledger = parse_cargo_installs(LEDGER).unwrap();
    let (status, msg) = provenance_report(
        "tm",
        "1.3.1",
        &installed("tm"),
        Path::new(CARGO_BIN),
        Some(&ledger),
        &always_exists,
    );
    assert_eq!(status, CheckStatus::Ok, "message: {msg}");
}

/// A dev build run straight out of `target/debug` is also not the cargo
/// install, and must report UNKNOWN rather than borrow the record.
#[test]
fn unknown_for_a_build_run_in_place() {
    let ledger = parse_cargo_installs(LEDGER).unwrap();
    let (status, _msg) = provenance_report(
        "tm",
        "1.3.1",
        Path::new("/repo/target/debug/tm"),
        Path::new(CARGO_BIN),
        Some(&ledger),
        &always_exists,
    );
    assert_eq!(status, CheckStatus::Unknown);
}
