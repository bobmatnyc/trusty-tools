//! Tests for the cargo-audit CVE-scan collector (#6075).
//!
//! Why: the collector is fail-open, so every arm that produces NOTHING has to
//! be pinned by a test that reads the gap it produced instead. A regression
//! here does not fail a build — it ships a report whose empty CVE section reads
//! as a clean dependency set.
//! What: the fixture parse, the two error arms #6075 names explicitly, the
//! applicability ladder, and the manifest write-back.
//! Test: this file.

use super::*;
use std::path::PathBuf;

/// A captured `cargo audit --json` document: one vulnerability, one
/// unmaintained warning, one yanked crate with no advisory record.
const FIXTURE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/testdata/cargo-audit.json"
));

/// A manifest with the one `[report]` table `write_into` edits.
const MANIFEST: &str = "[report]\ntitle = \"Acme\"\n\n[[repositories]]\nname = \"acme-api\"\npath = \"/tmp/acme-api\"\n";

/// A run that never happened: the seam every failure-arm test injects.
fn refuses(reason: &'static str) -> impl FnOnce(&Path) -> Result<Run, String> {
    move |_| Err(reason.to_string())
}

/// A run that completed, with the streams and status the test wants to assert.
fn returns(
    success: bool,
    stdout: &'static str,
    stderr: &'static str,
) -> impl FnOnce(&Path) -> Result<Run, String> {
    move |_| {
        Ok(Run {
            success,
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
        })
    }
}

/// A checkout with a `Cargo.lock`, so the ladder reaches the subprocess seam.
fn locked_checkout(tmp: &Path) -> PathBuf {
    let checkout = tmp.join("repos").join("acme-api");
    std::fs::create_dir_all(&checkout).expect("mkdir checkout");
    std::fs::write(checkout.join("Cargo.toml"), "[workspace]\n").expect("manifest");
    std::fs::write(checkout.join("Cargo.lock"), "version = 4\n").expect("lockfile");
    checkout
}

/// A manifest file the write-back can edit.
fn manifest_at(tmp: &Path) -> PathBuf {
    let path = tmp.join("manifest.toml");
    std::fs::write(&path, MANIFEST).expect("write manifest");
    path
}

// ─── Parsing ────────────────────────────────────────────────────────────────

#[test]
fn the_fixture_yields_every_row() {
    let rows = parse(FIXTURE).expect("the captured document parses");
    assert_eq!(rows.len(), 3, "{rows:?}");
}

/// The six fields #6075 requires, read off a real vulnerability row.
#[test]
fn a_vulnerability_becomes_a_red_advisory() {
    let rows = parse(FIXTURE).expect("parses");
    let found = rows
        .iter()
        .find(|a| a.id == "RUSTSEC-2024-0421")
        .unwrap_or_else(|| panic!("the vulnerability row is missing: {rows:?}"));
    assert_eq!(found.package, "idna");
    assert_eq!(found.version, "0.5.0");
    assert_eq!(found.severity, Severity::Red);
    assert!(found.title.contains("Punycode"), "{}", found.title);
    assert_eq!(
        found.url.as_deref(),
        Some("https://github.com/servo/rust-url/pull/1017"),
        "the advisory's own reference is preferred over a derived one"
    );
}

#[test]
fn a_warning_becomes_an_amber_advisory() {
    let rows = parse(FIXTURE).expect("parses");
    let found = rows
        .iter()
        .find(|a| a.id == "RUSTSEC-2024-0413")
        .unwrap_or_else(|| panic!("the unmaintained row is missing: {rows:?}"));
    assert_eq!(found.severity, Severity::Amber);
    assert_eq!(found.package, "atk");
}

/// A yanked crate has no advisory record at all, and must still be reported —
/// dropping it would be exactly the silent zero this collector exists to stop.
#[test]
fn a_yanked_crate_with_no_advisory_record_is_still_reported() {
    let rows = parse(FIXTURE).expect("parses");
    let found = rows
        .iter()
        .find(|a| a.package == "ghost-crate")
        .unwrap_or_else(|| panic!("the yanked row is missing: {rows:?}"));
    assert_eq!(found.id, "YANKED", "the kind stands in for the absent id");
    assert_eq!(found.version, "1.0.1");
    assert_eq!(found.severity, Severity::Amber);
    assert!(found.url.is_none(), "there is no advisory page to link");
}

#[test]
fn output_that_is_not_json_is_a_reason() {
    let cause = parse("error: no such subcommand").expect_err("not JSON");
    assert!(cause.contains("not readable as JSON"), "{cause}");
}

#[test]
fn json_that_is_not_cargo_audits_is_a_reason() {
    let cause = parse(r#"{"ok":true}"#).expect_err("wrong document");
    assert!(cause.contains("`vulnerabilities`"), "{cause}");
}

// ─── The applicability ladder ───────────────────────────────────────────────

/// #6075: a non-Rust target is a NAMED gap, never a silent zero-findings
/// result, and the gap names the language rather than the marker file.
#[test]
fn a_non_rust_repository_names_its_language() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let checkout = tmp.path().join("web");
    std::fs::create_dir_all(&checkout).expect("mkdir");
    std::fs::write(checkout.join("package.json"), "{}").expect("manifest");

    let Outcome::Unavailable(reason) = scan_with(&checkout, refuses("must not spawn")) else {
        panic!("a JavaScript repository has CVE exposure this collector cannot measure");
    };
    assert_eq!(
        reason, "cve-scan: no cargo-audit-equivalent for JavaScript/TypeScript",
        "the gap states the language"
    );
}

#[test]
fn a_rust_repository_with_no_lockfile_says_so() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let checkout = tmp.path().join("lib");
    std::fs::create_dir_all(&checkout).expect("mkdir");
    std::fs::write(checkout.join("Cargo.toml"), "[package]\nname = \"x\"\n").expect("manifest");

    let Outcome::Unavailable(reason) = scan_with(&checkout, refuses("must not spawn")) else {
        panic!("an unlocked Cargo project is unscannable, not inapplicable");
    };
    assert!(reason.contains("no Cargo.lock"), "{reason}");
}

/// A repository declaring no dependency manifest at all has no CVE surface to
/// claim is missing — the same declared-skip distinction `topology` draws.
#[test]
fn a_repository_with_no_manifest_is_a_declared_skip() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let checkout = tmp.path().join("docs");
    std::fs::create_dir_all(&checkout).expect("mkdir");

    assert!(
        matches!(
            scan_with(&checkout, refuses("must not spawn")),
            Outcome::NotApplicable(_)
        ),
        "a manifest-less repository is a skip, not a degradation"
    );
    assert!(
        ground_into_with(
            &manifest_at(tmp.path()),
            &checkout,
            "docs",
            refuses("must not spawn")
        )
        .is_empty(),
        "and it says nothing at all"
    );
}

// ─── Fail-open: the two error arms #6075 names ──────────────────────────────

/// Error arm 1. Fails before the collector existed by producing nothing at all;
/// fails against a silent implementation by producing an empty findings list.
#[test]
fn an_uninstalled_binary_is_a_named_gap() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let checkout = locked_checkout(tmp.path());
    let manifest = manifest_at(tmp.path());

    let missing = format!(
        "cve-scan: `cargo-audit` is not installed, so no dependency CVE scan ran (install it with \
         `{INSTALL_COMMAND}`)"
    );
    let leaked: &'static str = Box::leak(missing.into_boxed_str());
    let gaps = ground_into_with(&manifest, &checkout, "acme-api", refuses(leaked));

    assert_eq!(gaps.len(), 1, "{gaps:?}");
    assert!(gaps[0].starts_with("acme-api: cve-scan:"), "{}", gaps[0]);
    assert!(gaps[0].contains("cargo-audit"), "{}", gaps[0]);
    assert!(
        gaps[0].contains("cargo install cargo-audit"),
        "the gap names the install command: {}",
        gaps[0]
    );
    assert!(
        gaps[0].contains("unassessed rather than as a clean dependency set"),
        "{}",
        gaps[0]
    );
    assert_eq!(
        std::fs::read_to_string(&manifest).expect("read back"),
        MANIFEST,
        "no findings list — not even an empty one — is recorded for a scan that never ran"
    );
}

/// Error arm 2. `cargo audit` exits non-zero on finding vulnerabilities too, so
/// the status alone cannot condemn a run: what condemns this one is that its
/// stdout is not a document, and the gap says both.
#[test]
fn a_nonzero_exit_with_malformed_json_is_a_named_gap() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let checkout = locked_checkout(tmp.path());
    let manifest = manifest_at(tmp.path());

    let gaps = ground_into_with(
        &manifest,
        &checkout,
        "acme-api",
        returns(
            false,
            "{\"vulnerabilities\": ",
            "error: couldn't fetch advisory database\n",
        ),
    );

    assert_eq!(gaps.len(), 1, "{gaps:?}");
    assert!(gaps[0].contains("not readable as JSON"), "{}", gaps[0]);
    assert!(
        gaps[0].contains("exited non-zero"),
        "the exit status is named so a crash is distinguishable from a format change: {}",
        gaps[0]
    );
    assert!(
        gaps[0].contains("couldn't fetch advisory database"),
        "and the tool's own first diagnostic is quoted: {}",
        gaps[0]
    );
    assert_eq!(
        std::fs::read_to_string(&manifest).expect("read back"),
        MANIFEST,
        "no findings list — not even an empty one — is recorded for a scan that failed"
    );
}

/// The inverse of the arm above, and the reason the status cannot be the test:
/// `cargo audit` exits 1 precisely when it has the result that matters most.
#[test]
fn a_nonzero_exit_with_readable_json_is_still_a_scan() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let checkout = locked_checkout(tmp.path());

    let Outcome::Scanned(rows) = scan_with(&checkout, returns(false, FIXTURE, "")) else {
        panic!("a non-zero exit carrying a readable document is a successful scan");
    };
    assert_eq!(rows.len(), 3, "{rows:?}");
}

/// A clean scan and an absent scan must not read the same on the page, and the
/// clean one states what it did NOT cover rather than claiming a clean tree.
#[test]
fn a_clean_scan_states_its_own_scope() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let checkout = locked_checkout(tmp.path());
    let manifest = manifest_at(tmp.path());

    let clean = r#"{"vulnerabilities":{"found":false,"count":0,"list":[]},"warnings":{}}"#;
    let gaps = ground_into_with(&manifest, &checkout, "acme-api", returns(true, clean, ""));

    assert_eq!(gaps.len(), 1, "{gaps:?}");
    assert!(gaps[0].contains("reported no advisory"), "{}", gaps[0]);
    assert!(gaps[0].contains("Vendored code"), "{}", gaps[0]);
    assert_eq!(
        std::fs::read_to_string(&manifest).expect("read back"),
        MANIFEST,
        "an empty scan writes no empty table for trusty-review to render"
    );
}

// ─── The manifest write-back ────────────────────────────────────────────────

#[test]
fn the_advisories_land_in_the_manifest() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let checkout = locked_checkout(tmp.path());
    let manifest = manifest_at(tmp.path());

    let gaps = ground_into_with(&manifest, &checkout, "acme-api", returns(true, FIXTURE, ""));
    assert!(
        gaps.is_empty(),
        "a scan that wrote its rows says nothing: {gaps:?}"
    );

    let written = std::fs::read_to_string(&manifest).expect("read back");
    let parsed: toml::Value = toml::from_str(&written).expect("still valid TOML");
    let declared = parsed["report"]["findings"]
        .as_array()
        .expect("findings were declared");
    assert_eq!(declared.len(), 3, "{written}");

    let first = &declared[0];
    assert_eq!(first["category"].as_str(), Some("dependencies"));
    assert_eq!(first["id"].as_str(), Some("RUSTSEC-2024-0421"));
    assert_eq!(first["package"].as_str(), Some("idna"));
    assert_eq!(first["version"].as_str(), Some("0.5.0"));
    assert_eq!(first["severity"].as_str(), Some("RED"));
    assert!(
        first["title"]
            .as_str()
            .expect("a title")
            .contains("Punycode")
    );
    assert!(
        first["url"]
            .as_str()
            .expect("a url")
            .starts_with("https://")
    );

    assert_eq!(
        parsed["repositories"].as_array().expect("array").len(),
        1,
        "the repository list this crate does not own is untouched: {written}"
    );
}

/// A sweep resumed over a manifest it already wrote must not restate a row —
/// the same rule `priority::write_into` applies to `[report].gaps`.
#[test]
fn a_resumed_sweep_does_not_duplicate_a_row() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let manifest = manifest_at(tmp.path());
    let rows = parse(FIXTURE).expect("parses");

    write_into(&manifest, &rows).expect("first write");
    write_into(&manifest, &rows).expect("second write");

    let written = std::fs::read_to_string(&manifest).expect("read back");
    let parsed: toml::Value = toml::from_str(&written).expect("valid TOML");
    assert_eq!(
        parsed["report"]["findings"]
            .as_array()
            .expect("array")
            .len(),
        3,
        "{written}"
    );
}

#[test]
fn a_manifest_that_cannot_be_read_is_a_named_gap() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let checkout = locked_checkout(tmp.path());
    let manifest = tmp.path().join("nowhere").join("manifest.toml");

    let gaps = ground_into_with(&manifest, &checkout, "acme-api", returns(true, FIXTURE, ""));
    assert_eq!(gaps.len(), 1, "{gaps:?}");
    assert!(gaps[0].contains("could not be read"), "{}", gaps[0]);
    assert!(
        gaps[0].contains("3 advisory/ies"),
        "the gap states what was lost: {}",
        gaps[0]
    );
}
