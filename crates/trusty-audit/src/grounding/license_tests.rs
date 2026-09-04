//! Tests for the cargo-deny license-review collector (#6076).
//!
//! Why: the collector is fail-open, so every arm that produces NOTHING has to
//! be pinned by a test that reads the gap it produced instead. A regression
//! here does not fail a build — it ships a report whose empty License / IP
//! Exposure section reads as a permissively licensed dependency graph, which is
//! the false clean claim epic #6074 exists to remove.
//! What: the policy bands, the two fixtures #6076 asks for (a problematic
//! dependency set and a permissive one), the applicability ladder, every
//! failure arm, and the manifest write-back.
//! Test: this file.

use super::*;
use std::path::PathBuf;

/// A `cargo-deny list --layout crate` document carrying one crate per band:
/// unlicensed, strong copyleft, weak copyleft, an unrecognised term, a
/// `GPL OR MIT` dual license, and three plainly permissive crates.
const FIXTURE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/testdata/cargo-deny-list.json"
));

/// The same shape with nothing but permissive terms — #6076's second path.
const PERMISSIVE_FIXTURE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/testdata/cargo-deny-list-permissive.json"
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

/// The crate named, as [`classify`] banded it.
fn finding_for(json: &str, package: &str) -> Option<Finding> {
    parse(json)
        .expect("the fixture parses")
        .iter()
        .filter_map(classify)
        .find(|f| f.package == package)
}

// ─── Parsing ────────────────────────────────────────────────────────────────

#[test]
fn the_fixture_yields_every_crate() {
    let packages = parse(FIXTURE).expect("the captured document parses");
    assert_eq!(packages.len(), 8, "{packages:?}");
    let dual = packages
        .iter()
        .find(|p| p.name == "readerwriterqueue")
        .unwrap_or_else(|| panic!("the dual-licensed row is missing: {packages:?}"));
    assert_eq!(
        dual.version, "1.0.6",
        "the version is the key's second field"
    );
    assert_eq!(dual.licenses, ["GPL-2.0-only", "MIT"]);
}

#[test]
fn output_that_is_not_json_is_a_reason() {
    let cause = parse("error: no such subcommand").expect_err("not JSON");
    assert!(cause.contains("not readable as JSON"), "{cause}");
}

#[test]
fn json_that_is_not_the_crate_layout_is_a_reason() {
    let cause = parse(r#"["MIT (1): acme"]"#).expect_err("wrong layout");
    assert!(cause.contains("crate-layout listing object"), "{cause}");
}

// ─── The policy ─────────────────────────────────────────────────────────────

/// The finding an acquirer's counsel acts on: one AGPL crate binds the work.
#[test]
fn a_strong_copyleft_dependency_is_a_red_finding() {
    let found = finding_for(FIXTURE, "libfoo-sys").expect("AGPL is a finding");
    assert_eq!(found.license, "AGPL-3.0-or-later");
    assert_eq!(found.version, "0.9.1");
    assert_eq!(found.severity, Severity::Red);
    assert!(found.obligation.contains("same license"), "{found:?}");
    assert_eq!(
        found.url.as_deref(),
        Some("https://spdx.org/licenses/AGPL-3.0-or-later.html"),
        "a recognised identifier links to its SPDX page"
    );
}

#[test]
fn a_weak_copyleft_dependency_is_an_amber_finding() {
    let found = finding_for(FIXTURE, "colored").expect("MPL-2.0 is a finding");
    assert_eq!(found.license, "MPL-2.0");
    assert_eq!(found.severity, Severity::Amber);
    assert!(found.obligation.contains("file-level"), "{found:?}");
}

/// A crate declaring nothing is worse than any copyleft term: there is no
/// stated right to redistribute it at all.
#[test]
fn a_crate_with_no_license_is_the_worst_finding() {
    let found = finding_for(FIXTURE, "acme-core").expect("an unlicensed crate is a finding");
    assert_eq!(found.license, UNLICENSED);
    assert_eq!(found.severity, Severity::Red);
    assert!(found.obligation.contains("no license"), "{found:?}");
    assert!(
        found.url.is_none(),
        "UNLICENSED is not an SPDX identifier and gets no link: {found:?}"
    );
}

/// `GPL-2.0-only OR MIT` costs the acquirer nothing — they take the MIT option.
/// Flagging it would put a copyleft row in every report that depends on a
/// dual-licensed crate, which is most of them.
#[test]
fn a_dual_licensed_crate_takes_its_permissive_option() {
    assert!(
        finding_for(FIXTURE, "readerwriterqueue").is_none(),
        "a permissive term in the set clears the crate"
    );
    assert!(
        finding_for(FIXTURE, "aho-corasick").is_none(),
        "Unlicense/MIT is permissive on both terms"
    );
}

/// A term the policy table has never seen is the case that most needs a human,
/// so it is reported rather than dropped — dropping it is the silent zero.
#[test]
fn an_unrecognised_term_is_reported_rather_than_dropped() {
    let found = finding_for(FIXTURE, "vendor-blob").expect("an unknown term is a finding");
    assert_eq!(found.license, "LicenseRef-Acme-Commercial");
    assert_eq!(found.severity, Severity::Amber);
    assert!(found.obligation.contains("reviewed by hand"), "{found:?}");
    assert!(
        found.url.is_none(),
        "no SPDX link is invented for a term SPDX does not define: {found:?}"
    );
}

/// #6076's second closure condition, at the policy layer: a permissive-only
/// dependency set produces no finding at all.
#[test]
fn a_permissive_dependency_set_produces_no_finding() {
    let findings: Vec<Finding> = parse(PERMISSIVE_FIXTURE)
        .expect("parses")
        .iter()
        .filter_map(classify)
        .collect();
    assert!(findings.is_empty(), "{findings:?}");
}

/// `Apache-2.0 WITH LLVM-exception`, `GPL-3.0-only` and `GPL-3.0+` are all
/// spellings the policy tables do not list literally.
#[test]
fn spdx_qualifiers_do_not_defeat_the_policy() {
    let package = |licenses: &[&str]| Package {
        name: "x".to_string(),
        version: "1.0.0".to_string(),
        licenses: licenses.iter().map(|s| (*s).to_string()).collect(),
    };
    assert!(
        classify(&package(&["Apache-2.0 WITH LLVM-exception"])).is_none(),
        "a WITH exception does not hide a permissive base"
    );
    for spelling in ["GPL-3.0-only", "GPL-3.0-or-later", "GPL-3.0+", "gpl-3.0"] {
        let found = classify(&package(&[spelling]))
            .unwrap_or_else(|| panic!("{spelling} must still band as copyleft"));
        assert_eq!(found.severity, Severity::Red, "{spelling}");
    }
}

/// A whole expression arriving as ONE license term, which `normalise` cannot
/// take apart.
///
/// Why: pre-SPDX crate metadata wrote a dual license as the slash-joined
/// `MIT/Apache-2.0`, and a vendored or hand-edited manifest still can — while
/// `cargo-deny list` normally flattens an expression into separate terms, this
/// module must not assume it always did. The permissive halves inside such a
/// string are invisible to the policy tables, and the safe direction is to
/// UNDER-clear: report the crate for a human rather than clear it on a
/// substring nobody parsed. What must not happen is a panic, a silent clear, or
/// an SPDX link built from a string SPDX does not define — `spdx_url` returning
/// `Some` here would send the reader to an authoritative-looking 404.
#[test]
fn an_unparsed_compound_expression_is_reported_rather_than_cleared() {
    let package = |license: &str| Package {
        name: "x".to_string(),
        version: "1.0.0".to_string(),
        licenses: vec![license.to_string()],
    };
    // The legacy slash form, and a compound spelling no table lists.
    for spelling in ["MIT/Apache-2.0", "Apache-2.0 OR LGPL-3.0"] {
        let found = classify(&package(spelling))
            .unwrap_or_else(|| panic!("{spelling} must not clear the crate"));
        assert_eq!(found.license, spelling, "reported verbatim: {found:?}");
        assert_eq!(found.severity, Severity::Amber, "{spelling}");
        assert!(
            found.obligation.contains("reviewed by hand"),
            "{spelling}: {found:?}"
        );
        assert!(
            found.url.is_none(),
            "{spelling} is not an SPDX identifier and must get no link: {found:?}"
        );
    }
}

/// One row per crate at most, so the rendered table can never outgrow the
/// dependency list it was built from.
#[test]
fn a_crate_earns_at_most_one_row() {
    let packages = parse(FIXTURE).expect("parses");
    let findings: Vec<Finding> = packages.iter().filter_map(classify).collect();
    assert!(findings.len() <= packages.len(), "{findings:?}");
    assert_eq!(findings.len(), 4, "{findings:?}");
}

// ─── The applicability ladder ───────────────────────────────────────────────

/// #6076: a non-Rust target is a NAMED gap, never a silent zero-findings
/// result, and the gap names the language rather than the marker file.
#[test]
fn a_non_rust_repository_names_its_language() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let checkout = tmp.path().join("web");
    std::fs::create_dir_all(&checkout).expect("mkdir");
    std::fs::write(checkout.join("go.mod"), "module acme\n").expect("manifest");

    let Outcome::Unavailable(reason) = review_with(&checkout, refuses("must not spawn")) else {
        panic!("a Go repository has license exposure this collector cannot measure");
    };
    assert_eq!(
        reason, "license-review: no cargo-deny-equivalent for Go",
        "the gap states the language"
    );
}

#[test]
fn a_rust_repository_with_no_lockfile_says_so() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let checkout = tmp.path().join("lib");
    std::fs::create_dir_all(&checkout).expect("mkdir");
    std::fs::write(checkout.join("Cargo.toml"), "[package]\nname = \"x\"\n").expect("manifest");

    let Outcome::Unavailable(reason) = review_with(&checkout, refuses("must not spawn")) else {
        panic!("an unlocked Cargo project is unreviewable, not inapplicable");
    };
    assert!(reason.contains("no Cargo.lock"), "{reason}");
}

/// A repository declaring no dependency manifest at all has no license surface
/// to claim is missing — the declared-skip distinction `cve` and `topology`
/// both draw.
#[test]
fn a_repository_with_no_manifest_is_a_declared_skip() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let checkout = tmp.path().join("docs");
    std::fs::create_dir_all(&checkout).expect("mkdir");

    assert!(
        matches!(
            review_with(&checkout, refuses("must not spawn")),
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

// ─── Fail-open: every arm that produces no findings ─────────────────────────

/// Error arm 1, and #6076's Fail-Open Check. Fails before the collector existed
/// by producing nothing at all; fails against a silent implementation by
/// producing an empty findings list and no gap.
#[test]
fn an_uninstalled_binary_is_a_named_gap() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let checkout = locked_checkout(tmp.path());
    let manifest = manifest_at(tmp.path());

    let missing = format!(
        "license-review: `cargo-deny` is not installed, so no dependency license review ran \
         (install it with `{INSTALL_COMMAND}`)"
    );
    let leaked: &'static str = Box::leak(missing.into_boxed_str());
    let gaps = ground_into_with(&manifest, &checkout, "acme-api", refuses(leaked));

    assert_eq!(gaps.len(), 1, "{gaps:?}");
    assert!(
        gaps[0].starts_with("acme-api: license-review:"),
        "{}",
        gaps[0]
    );
    assert!(gaps[0].contains("cargo-deny"), "{}", gaps[0]);
    assert!(
        gaps[0].contains("cargo install cargo-deny"),
        "the gap names the install command: {}",
        gaps[0]
    );
    assert!(
        gaps[0].contains("unassessed rather than as a permissively licensed dependency set"),
        "{}",
        gaps[0]
    );
    assert_eq!(
        std::fs::read_to_string(&manifest).expect("read back"),
        MANIFEST,
        "no findings list — not even an empty one — is recorded for a review that never ran"
    );
}

/// Error arm 2. `cargo-deny list` exits zero whenever it produced a listing, so
/// a non-zero exit is a failure and never a result — the opposite of `cargo
/// audit`, whose non-zero exit IS its most important finding.
#[test]
fn a_nonzero_exit_is_a_named_gap() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let checkout = locked_checkout(tmp.path());
    let manifest = manifest_at(tmp.path());

    let gaps = ground_into_with(
        &manifest,
        &checkout,
        "acme-api",
        returns(
            false,
            "",
            "error: failed to read lock file: not a lock file\n",
        ),
    );

    assert_eq!(gaps.len(), 1, "{gaps:?}");
    assert!(
        gaps[0].contains("exited non-zero without a listing"),
        "{}",
        gaps[0]
    );
    assert!(
        gaps[0].contains("failed to read lock file"),
        "the gap carries the tool's own cause, not a generic one: {}",
        gaps[0]
    );
    assert_eq!(
        std::fs::read_to_string(&manifest).expect("read back"),
        MANIFEST,
        "the manifest is byte-identical after a failed review"
    );
}

/// #6720: a leading informational line must not become "the reason". Here the
/// cause this module determined leads, and the tool's stderr follows it — so an
/// unhelpful first line costs detail, never the diagnosis.
#[test]
fn a_noisy_first_stderr_line_does_not_replace_the_diagnosis() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let checkout = locked_checkout(tmp.path());
    let manifest = manifest_at(tmp.path());

    let gaps = ground_into_with(
        &manifest,
        &checkout,
        "acme-api",
        returns(
            false,
            "",
            "Update available: cargo-deny 0.21.0\nerror: real cause\n",
        ),
    );

    assert_eq!(gaps.len(), 1, "{gaps:?}");
    assert!(
        gaps[0].contains("license-review: `cargo-deny` exited non-zero without a listing"),
        "the diagnosis is this module's own, stated before any borrowed stderr: {}",
        gaps[0]
    );
}

/// A zero exit whose stdout is unreadable is still a failure, and the gap says
/// which of the two it was.
#[test]
fn unreadable_output_is_a_named_gap() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let checkout = locked_checkout(tmp.path());
    let manifest = manifest_at(tmp.path());

    let gaps = ground_into_with(
        &manifest,
        &checkout,
        "acme-api",
        returns(true, "<html>", ""),
    );

    assert_eq!(gaps.len(), 1, "{gaps:?}");
    assert!(gaps[0].contains("not readable as JSON"), "{}", gaps[0]);
    assert_eq!(
        std::fs::read_to_string(&manifest).expect("read back"),
        MANIFEST,
        "the manifest is byte-identical after an unreadable review"
    );
}

/// #6076's second closure condition end to end: a permissive-only target writes
/// no findings AND still says what was reviewed, so the empty License section
/// is never read as an unrun scan.
#[test]
fn a_clean_review_states_its_own_scope() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let checkout = locked_checkout(tmp.path());
    let manifest = manifest_at(tmp.path());

    let gaps = ground_into_with(
        &manifest,
        &checkout,
        "acme-api",
        returns(true, PERMISSIVE_FIXTURE, ""),
    );

    assert_eq!(gaps.len(), 1, "{gaps:?}");
    assert!(
        gaps[0].contains("every dependency offers a permissive license"),
        "{}",
        gaps[0]
    );
    assert!(
        gaps[0].contains("Vendored code"),
        "the clean line states what it did NOT cover: {}",
        gaps[0]
    );
    assert_eq!(
        std::fs::read_to_string(&manifest).expect("read back"),
        MANIFEST,
        "an empty findings array is never written"
    );
}

#[test]
fn a_manifest_that_cannot_be_written_is_a_named_gap() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let checkout = locked_checkout(tmp.path());
    let missing = tmp.path().join("nowhere").join("manifest.toml");

    let gaps = ground_into_with(&missing, &checkout, "acme-api", returns(true, FIXTURE, ""));

    assert_eq!(gaps.len(), 1, "{gaps:?}");
    assert!(gaps[0].contains("could not be read"), "{}", gaps[0]);
    assert!(
        gaps[0].contains("license obligation(s)"),
        "the gap says how many rows the report is therefore missing: {}",
        gaps[0]
    );
}

// ─── The manifest write-back ────────────────────────────────────────────────

/// #6076's first closure condition: the findings reach `[report].findings`
/// under the `license` category trusty-review renders.
#[test]
fn the_findings_land_in_the_manifest() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let checkout = locked_checkout(tmp.path());
    let manifest = manifest_at(tmp.path());

    let gaps = ground_into_with(&manifest, &checkout, "acme-api", returns(true, FIXTURE, ""));
    assert!(
        gaps.is_empty(),
        "a review that wrote its rows says nothing: {gaps:?}"
    );

    let written = std::fs::read_to_string(&manifest).expect("read back");
    assert!(written.contains("category = \"license\""), "{written}");
    assert!(written.contains("id = \"AGPL-3.0-or-later\""), "{written}");
    assert!(written.contains("package = \"libfoo-sys\""), "{written}");
    assert!(written.contains("severity = \"RED\""), "{written}");
    assert!(written.contains("id = \"MPL-2.0\""), "{written}");
    assert!(written.contains("id = \"UNLICENSED\""), "{written}");
    assert!(
        written.starts_with("[report]\ntitle = \"Acme\""),
        "the edit is format-preserving: {written}"
    );
    assert!(
        written.contains("[[repositories]]"),
        "nothing else in the document changed: {written}"
    );

    // The rows are what trusty-review deserialises into `ManifestFinding`.
    let doc: toml::Value = written.parse().expect("the edited manifest is valid TOML");
    let rows = doc["report"]["findings"]
        .as_array()
        .expect("findings is an array");
    assert_eq!(rows.len(), 4, "{rows:?}");
    assert!(
        rows.iter()
            .all(|r| r["category"].as_str() == Some("license")),
        "{rows:?}"
    );
}

#[test]
fn a_resumed_sweep_does_not_duplicate_a_row() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let checkout = locked_checkout(tmp.path());
    let manifest = manifest_at(tmp.path());

    let _ = ground_into_with(&manifest, &checkout, "acme-api", returns(true, FIXTURE, ""));
    let once = std::fs::read_to_string(&manifest).expect("read back");
    let _ = ground_into_with(&manifest, &checkout, "acme-api", returns(true, FIXTURE, ""));
    let twice = std::fs::read_to_string(&manifest).expect("read back");

    assert_eq!(once, twice, "the second pass restates nothing");
}

/// The channel is shared with #6075: a CVE row already in the array must
/// survive a license write, and must not be mistaken for one of its rows.
#[test]
fn a_cve_row_already_present_is_left_alone() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let checkout = locked_checkout(tmp.path());
    let manifest = tmp.path().join("manifest.toml");
    std::fs::write(
        &manifest,
        "[report]\ntitle = \"Acme\"\nfindings = [\n    { category = \"dependencies\", id = \
         \"RUSTSEC-2024-0421\", package = \"idna\", version = \"0.5.0\", severity = \"RED\", \
         title = \"Punycode\" },\n]\n\n[[repositories]]\nname = \"acme-api\"\npath = \
         \"/tmp/acme-api\"\n",
    )
    .expect("seed manifest");

    let gaps = ground_into_with(&manifest, &checkout, "acme-api", returns(true, FIXTURE, ""));
    assert!(gaps.is_empty(), "the seeded manifest is writable: {gaps:?}");

    let doc: toml::Value = std::fs::read_to_string(&manifest)
        .expect("read back")
        .parse()
        .expect("valid TOML");
    let rows = doc["report"]["findings"]
        .as_array()
        .expect("findings is an array");
    assert_eq!(
        rows.len(),
        5,
        "the CVE row plus four license rows: {rows:?}"
    );
    assert_eq!(
        rows.iter()
            .filter(|r| r["category"].as_str() == Some("dependencies"))
            .count(),
        1,
        "the CVE row is untouched: {rows:?}"
    );
}
