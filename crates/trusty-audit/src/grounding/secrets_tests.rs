//! Tests for the gitleaks secrets collector (#6077).
//!
//! Why: the collector is fail-open, so every arm that produces NOTHING has to be
//! pinned by a test that reads the gap it produced instead. A regression here
//! does not fail a build — it ships a report whose empty Secret Leakage section
//! reads as a tree with no credential in it, which is the false clean claim epic
//! #6074 exists to remove. The redaction has the same shape of failure: a
//! collector that writes the matched value would put a live credential into a
//! document that leaves the operator's machine, and nothing downstream checks.
//! What: the two fixtures #6077 asks for (a tree with planted synthetic secrets
//! and a clean one), the band table, every failure arm, the #6720 diagnosis-
//! leads rule, and the manifest write-back.
//! Test: this file.

use super::*;
use std::path::PathBuf;

/// A gitleaks report carrying one row per shape this collector must handle: a
/// provider credential, a generic entropy match, a symlinked private key, a row
/// naming no file at all, and a secret too short to preview.
const FIXTURE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/testdata/gitleaks-report.json"
));

/// The same shape with nothing in it — #6077's clean path.
const CLEAN_FIXTURE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/testdata/gitleaks-report-clean.json"
));

/// Every synthetic credential planted in [`FIXTURE`], verbatim.
///
/// The manifest must never contain any of them. They are deliberately not
/// provider-valid shapes (AWS's own documentation example, and strings with no
/// issuer prefix) so the fixture cannot be mistaken for a real leak.
const PLANTED: &[&str] = &[
    "AKIAIOSFODNN7EXAMPLE",
    "EXAMPLE0NOT0A0REAL0SECRET0VALUE000000",
    "SYNTHETICEXAMPLEKEYMATERIALNOTAKEY01",
    "SYNTHETICUNLOCATABLEVALUE0000",
];

/// A manifest with the one `[report]` table the write-back edits.
const MANIFEST: &str = "[report]\ntitle = \"Acme\"\n\n[[repositories]]\nname = \"acme-api\"\npath = \"/tmp/acme-api\"\n";

/// The stderr a target with a newer gitleaks on crates.io emits first (#6720).
const NOISY_STDERR: &str =
    "Update available: gitleaks 8.99.0 (you have 8.18.0)\nFTL failed to load config\n";

/// A run that never happened: the seam every failure-arm test injects.
fn refuses(reason: &'static str) -> impl FnOnce(&Path) -> Result<Run, String> {
    move |_| Err(reason.to_string())
}

/// A run that completed, with the report and status the test wants to assert.
fn returns(
    success: bool,
    report: &'static str,
    stderr: &'static str,
) -> impl FnOnce(&Path) -> Result<Run, String> {
    move |_| {
        Ok(Run {
            success,
            report: report.to_string(),
            stderr: stderr.to_string(),
        })
    }
}

/// A checkout directory, which is the whole applicability ladder for this leg.
fn checkout_at(tmp: &Path) -> PathBuf {
    let checkout = tmp.join("repos").join("acme-api");
    std::fs::create_dir_all(&checkout).expect("mkdir checkout");
    checkout
}

/// A manifest file the write-back can edit.
fn manifest_at(tmp: &Path) -> PathBuf {
    let path = tmp.join("manifest.toml");
    std::fs::write(&path, MANIFEST).expect("write manifest");
    path
}

/// The leaks [`FIXTURE`] declares, as [`parse`] reduces them.
fn fixture_leaks() -> Vec<Leak> {
    parse(FIXTURE, Path::new("/nowhere")).expect("the fixture parses")
}

/// The leak matching one rule id, for a band assertion.
fn leak_for(rule: &str) -> Leak {
    fixture_leaks()
        .into_iter()
        .find(|leak| leak.rule == rule)
        .unwrap_or_else(|| panic!("the fixture declares a {rule} row"))
}

// ─── Parsing ────────────────────────────────────────────────────────────────

/// Every row a reader could open becomes a leak, at the location gitleaks gave
/// it — #6077's `file:line` closure condition.
#[test]
fn the_fixture_yields_every_row() {
    let locations: Vec<String> = fixture_leaks().iter().map(Leak::location).collect();

    assert_eq!(
        locations,
        vec![
            "config/settings.yml:12".to_string(),
            "src/client.rs:88".to_string(),
            "deploy/id_rsa:1".to_string(),
            "scripts/deploy.sh:7".to_string(),
        ],
        "the symlinked row is located by its SymlinkFile, and the row naming no \
         file at all is skipped"
    );
}

/// A row with neither `File` nor `SymlinkFile` has no location a reader could
/// open, so it is dropped rather than reported at an empty path.
#[test]
fn a_row_naming_no_file_is_skipped() {
    assert!(
        !fixture_leaks()
            .iter()
            .any(|leak| leak.rule == "slack-bot-token"),
        "the fixture's unlocatable row must not reach the report"
    );
}

/// A provider's own credential format means a real credential is in the tree.
#[test]
fn a_provider_credential_bands_red() {
    assert_eq!(leak_for("aws-access-token").severity, Severity::Red);
    assert_eq!(leak_for("private-key").severity, Severity::Red);
}

/// The entropy heuristic means a human has to look, which is the AMBER band.
#[test]
fn a_generic_entropy_match_bands_amber() {
    assert_eq!(leak_for("generic-api-key").severity, Severity::Amber);
    assert_eq!(band("GENERIC-API-KEY"), Severity::Amber, "case-insensitive");
}

/// A rule the report names is used verbatim; a row with none still renders.
#[test]
fn a_row_with_no_rule_id_is_still_reported() {
    let leaks = parse(
        r#"[{"File":"a.rs","StartLine":3,"Secret":"SYNTHETIC0VALUE0HERE"}]"#,
        Path::new("/nowhere"),
    )
    .expect("parses");

    assert_eq!(leaks[0].rule, "unnamed-rule");
    assert_eq!(leaks[0].location(), "a.rs:3");
}

/// gitleaks states paths absolutely in some versions; the report must not carry
/// the operator's directory layout out of their machine.
#[test]
fn an_absolute_path_is_stated_relative_to_the_checkout() {
    let leaks = parse(
        r#"[{"File":"/opt/sweep/acme-api/src/main.rs","StartLine":9,"Secret":"SYNTHETIC0VALUE0HERE"}]"#,
        Path::new("/opt/sweep/acme-api"),
    )
    .expect("parses");

    assert_eq!(leaks[0].location(), "src/main.rs:9");
}

/// A secret at or under the redactor's head length is masked in full (#2475),
/// which this collector inherits rather than re-deciding.
#[test]
fn a_short_secret_is_masked_in_full() {
    let leak = fixture_leaks()
        .into_iter()
        .find(|leak| leak.file == "scripts/deploy.sh")
        .expect("the fixture declares a short-secret row");

    assert_eq!(leak.excerpt, "…(4 chars)", "no head is shown at all");
}

/// A document that is not JSON is a reason, not a panic and not a clean scan.
#[test]
fn output_that_is_not_json_is_a_reason() {
    let cause = parse("FTL failed to load config", Path::new("/nowhere"))
        .expect_err("prose is not a report");

    assert!(cause.contains("not readable as JSON"), "{cause}");
}

/// gitleaks emits an ARRAY; anything else is not its report.
#[test]
fn a_json_object_is_not_the_report_shape() {
    let cause = parse(r#"{"findings":[]}"#, Path::new("/nowhere"))
        .expect_err("an object is not the report");

    assert!(cause.contains("array of findings"), "{cause}");
}

/// An empty array is a clean scan, which is a result rather than an error.
#[test]
fn an_empty_report_parses_to_no_leaks() {
    assert!(
        parse(CLEAN_FIXTURE, Path::new("/nowhere"))
            .expect("the clean fixture parses")
            .is_empty()
    );
}

// ─── The redaction ──────────────────────────────────────────────────────────

/// The raw report is the one artefact holding the matched credentials, so it
/// lives where no other local account can read it and does not outlive the run.
#[cfg(unix)]
#[test]
fn the_raw_report_lives_in_a_private_directory_and_does_not_outlive_the_run() {
    use std::os::unix::fs::PermissionsExt;

    let dir = private_report_dir().expect("a private directory");
    let report = report_path_in(&dir);
    let root = dir.path().to_path_buf();
    let private = report
        .parent()
        .expect("the private subdirectory")
        .to_owned();
    let mode = std::fs::metadata(&private)
        .expect("stat")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        mode, PRIVATE_MODE,
        "gitleaks writes the credentials unredacted, so only this user may read \
         them — and `tempfile`'s own directory is 0755 under a 022 umask, which \
         is why the report lives one level down"
    );
    std::fs::write(&report, FIXTURE).expect("a report in it");
    drop(dir);
    assert!(
        !root.exists(),
        "the directory and the report inside it go with the drop, on every return \
         path from `run_gitleaks`"
    );

    // The REAL `run_gitleaks`, whichever arm this machine takes, leaves nothing.
    // Asserted in the same test rather than its own so no sibling's live
    // directory can race the sweep below.
    let tmp = tempfile::tempdir().expect("tempdir");
    let checkout = checkout_at(tmp.path());
    let _ = scan(&checkout);
    let lingering: Vec<PathBuf> = std::fs::read_dir(std::env::temp_dir())
        .expect("read the temp dir")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|entry| {
            entry
                .file_name()
                .and_then(std::ffi::OsStr::to_str)
                .is_some_and(|name| name.starts_with(REPORT_DIR_PREFIX))
        })
        .collect();
    assert!(
        lingering.is_empty(),
        "no raw report survives a scan: {lingering:?}"
    );
}

/// `Run` holds gitleaks' `Secret` and `Match` verbatim, so its `Debug` is
/// hand-written and must never print them — a derived one would put a live
/// credential into any `{:?}`, `tracing` field, or test panic.
#[test]
fn the_raw_run_never_debug_prints_its_content() {
    let run = Run {
        success: false,
        report: FIXTURE.to_string(),
        stderr: NOISY_STDERR.to_string(),
    };

    let rendered = format!("{run:?}");

    for planted in PLANTED {
        assert!(
            !rendered.contains(planted),
            "`Run`'s Debug printed the planted secret {planted}: {rendered}"
        );
    }
    assert!(
        !rendered.contains("Update available"),
        "the child's stderr is not this process's to echo either: {rendered}"
    );
    assert!(
        rendered.contains("bytes, withheld"),
        "the size still reaches a diagnostic: {rendered}"
    );
}

/// #6077's hardest requirement: the matched credential must never reach the
/// document that ships. Asserted against the MANIFEST, not against the struct,
/// because the manifest is what leaves the machine.
#[test]
fn the_manifest_never_carries_the_secret_value() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let manifest = manifest_at(tmp.path());

    write_into(&manifest, &fixture_leaks()).expect("the write succeeds");
    let written = std::fs::read_to_string(&manifest).expect("read back");

    for planted in PLANTED {
        assert!(
            !written.contains(planted),
            "the manifest carries the planted secret {planted}:\n{written}"
        );
    }
    assert!(
        !written.contains("aws_access_key_id = AKIA"),
        "gitleaks' `Match` field carries the value too, and is never written"
    );
    assert!(
        written.contains("redacted match AKIA…(20 chars)"),
        "the row still identifies which credential it found:\n{written}"
    );
}

// ─── The failure arms ───────────────────────────────────────────────────────

/// #6077's second closure condition: a target with no gitleaks names the reason
/// and what to install, rather than reporting a clean tree.
#[test]
fn an_uninstalled_binary_is_a_named_gap() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let checkout = checkout_at(tmp.path());
    let manifest = manifest_at(tmp.path());
    let reason = "secrets-scan: `gitleaks` is not installed, so no secrets scan ran (install it \
                  with `brew install gitleaks`)";

    let gaps = ground_into_with(&manifest, &checkout, "acme-api", refuses(reason));

    assert_eq!(gaps.len(), 1, "{gaps:?}");
    assert!(gaps[0].starts_with("acme-api: secrets-scan: `gitleaks` is not installed"));
    assert!(gaps[0].contains("brew install gitleaks"), "{}", gaps[0]);
    assert!(
        gaps[0].contains("must be read as unassessed"),
        "{}",
        gaps[0]
    );
    assert_manifest_unchanged(&manifest);
}

/// A spawn that failed for any other reason is its own named gap.
#[test]
fn a_spawn_failure_is_a_named_gap() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let checkout = checkout_at(tmp.path());
    let manifest = manifest_at(tmp.path());

    let gaps = ground_into_with(
        &manifest,
        &checkout,
        "acme-api",
        refuses("secrets-scan: `gitleaks` could not be run (Permission denied (os error 13))"),
    );

    assert_eq!(gaps.len(), 1, "{gaps:?}");
    assert!(gaps[0].contains("could not be run"), "{}", gaps[0]);
    assert_manifest_unchanged(&manifest);
}

/// A non-zero exit that left nothing to read is a failed run, NOT a clean tree.
#[test]
fn a_nonzero_exit_with_no_report_is_a_named_gap() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let checkout = checkout_at(tmp.path());
    let manifest = manifest_at(tmp.path());

    let gaps = ground_into_with(
        &manifest,
        &checkout,
        "acme-api",
        returns(false, "", "FTL failed to load config\n"),
    );

    assert_eq!(gaps.len(), 1, "{gaps:?}");
    assert!(
        gaps[0].contains("exited non-zero and left no findings report"),
        "{}",
        gaps[0]
    );
    assert!(gaps[0].contains("FTL failed to load config"), "{}", gaps[0]);
    assert_manifest_unchanged(&manifest);
}

/// A report that is not readable is a named gap even when the process exited 0.
#[test]
fn an_unreadable_report_is_a_named_gap() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let checkout = checkout_at(tmp.path());
    let manifest = manifest_at(tmp.path());

    let gaps = ground_into_with(
        &manifest,
        &checkout,
        "acme-api",
        returns(true, "<html>proxy error</html>", ""),
    );

    assert_eq!(gaps.len(), 1, "{gaps:?}");
    assert!(
        gaps[0].starts_with("acme-api: secrets-scan: `gitleaks` report is not readable as JSON")
    );
    assert_manifest_unchanged(&manifest);
}

/// gitleaks exits 1 when it FINDS leaks, so a readable report is a scan whatever
/// the exit status — and that is its most important result.
#[test]
fn a_nonzero_exit_with_a_readable_report_is_still_a_scan() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let checkout = checkout_at(tmp.path());

    let outcome = scan_with(&checkout, returns(false, FIXTURE, "WRN leaks found: 4\n"));

    match outcome {
        Outcome::Scanned(leaks) => assert_eq!(leaks.len(), 4),
        other => panic!("a readable report is a scan, got {other:?}"),
    }
}

/// #6720: the child's first stderr line is a parenthetical, never the reason.
/// An unrelated update notice must not become the whole diagnosis.
#[test]
fn a_noisy_stderr_never_replaces_the_diagnosis() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let checkout = checkout_at(tmp.path());
    let manifest = manifest_at(tmp.path());

    let gaps = ground_into_with(
        &manifest,
        &checkout,
        "acme-api",
        returns(false, "not json", NOISY_STDERR),
    );

    assert_eq!(gaps.len(), 1, "{gaps:?}");
    assert!(
        gaps[0].starts_with("acme-api: secrets-scan: `gitleaks` report is not readable as JSON"),
        "this module's own diagnosis leads: {}",
        gaps[0]
    );
    let notice = gaps[0]
        .find("Update available")
        .expect("the notice is still carried, as the parenthetical");
    let diagnosis = gaps[0]
        .find("not readable as JSON")
        .expect("the diagnosis is present");
    assert!(diagnosis < notice, "the diagnosis comes first: {}", gaps[0]);
    assert_manifest_unchanged(&manifest);
}

/// A path that is not a directory has no working tree, and says so.
#[test]
fn a_checkout_that_is_not_a_directory_is_a_named_gap() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let file = tmp.path().join("not-a-repo");
    std::fs::write(&file, "").expect("write");

    // The real `scan` — no seam — so this also proves the ladder short-circuits
    // before any subprocess is spawned.
    match scan(&file) {
        Outcome::Unavailable(cause) => {
            assert!(cause.contains("is not a directory"), "{cause}");
        }
        other => panic!("a file is not a checkout, got {other:?}"),
    }
}

/// #6077's clean path: no findings, and a scope statement naming what the scan
/// did NOT cover — so an empty Secret Leakage section is never read as proof.
#[test]
fn a_clean_scan_states_its_own_scope() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let checkout = checkout_at(tmp.path());
    let manifest = manifest_at(tmp.path());

    let gaps = ground_into_with(
        &manifest,
        &checkout,
        "acme-api",
        returns(true, CLEAN_FIXTURE, ""),
    );

    assert_eq!(gaps.len(), 1, "{gaps:?}");
    assert!(gaps[0].contains("matched no credential"), "{}", gaps[0]);
    assert!(gaps[0].contains("Git history"), "{}", gaps[0]);
    assert_manifest_unchanged(&manifest);
}

/// An empty report file after a clean exit is gitleaks reporting nothing, which
/// several 8.x versions do instead of writing `[]`.
#[test]
fn an_empty_report_after_a_clean_exit_is_a_clean_scan() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let checkout = checkout_at(tmp.path());

    match scan_with(&checkout, returns(true, "", "INF scan completed\n")) {
        Outcome::Scanned(leaks) => assert!(leaks.is_empty()),
        other => panic!("an empty report after exit 0 is clean, got {other:?}"),
    }
}

/// A manifest that cannot be read costs this repository's rows and says how
/// many, rather than failing the sweep.
#[test]
fn a_manifest_that_cannot_be_read_is_a_named_gap() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let checkout = checkout_at(tmp.path());
    let missing = tmp.path().join("nowhere").join("manifest.toml");

    let gaps = ground_into_with(&missing, &checkout, "acme-api", returns(true, FIXTURE, ""));

    assert_eq!(gaps.len(), 1, "{gaps:?}");
    assert!(gaps[0].contains("could not be read"), "{}", gaps[0]);
    assert!(
        gaps[0].contains("none of the 4 leak(s)"),
        "the gap says how many rows the report is therefore missing: {}",
        gaps[0]
    );
}

/// A manifest that reads but cannot be written back is a named gap, and the file
/// on disk is left byte-identical rather than half-written.
#[cfg(unix)]
#[test]
fn a_manifest_that_cannot_be_written_is_a_named_gap() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().expect("tempdir");
    let checkout = checkout_at(tmp.path());
    let manifest = manifest_at(tmp.path());
    std::fs::set_permissions(&manifest, std::fs::Permissions::from_mode(0o444))
        .expect("make the manifest read-only");

    let gaps = ground_into_with(&manifest, &checkout, "acme-api", returns(true, FIXTURE, ""));

    assert_eq!(gaps.len(), 1, "{gaps:?}");
    assert!(gaps[0].contains("could not be written"), "{}", gaps[0]);
    assert_manifest_unchanged(&manifest);
}

// ─── The manifest write-back ────────────────────────────────────────────────

/// #6077's first closure condition: the leaks reach `[report].findings` under
/// the `secrets` category trusty-review already renders as Secret Leakage.
#[test]
fn the_leaks_land_in_the_manifest() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let checkout = checkout_at(tmp.path());
    let manifest = manifest_at(tmp.path());

    let gaps = ground_into_with(
        &manifest,
        &checkout,
        "acme-api",
        returns(false, FIXTURE, ""),
    );

    assert!(
        gaps.is_empty(),
        "a scan that wrote its rows says nothing: {gaps:?}"
    );
    let written = std::fs::read_to_string(&manifest).expect("read back");
    assert_eq!(
        written.matches("category = \"secrets\"").count(),
        4,
        "{written}"
    );
    assert!(written.contains("id = \"aws-access-token\""), "{written}");
    assert!(
        written.contains("package = \"config/settings.yml:12\""),
        "the row carries file:line: {written}"
    );
    assert!(written.contains("severity = \"RED\""), "{written}");
    assert!(written.contains("severity = \"AMBER\""), "{written}");
    assert!(
        written.contains("title = \"Acme\""),
        "the document's own keys survive: {written}"
    );
}

/// A resumed sweep re-runs the collector against a manifest that already carries
/// its rows; it must not restate them.
#[test]
fn a_resumed_sweep_does_not_restate_a_leak() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let manifest = manifest_at(tmp.path());
    let leaks = fixture_leaks();

    write_into(&manifest, &leaks).expect("first write");
    write_into(&manifest, &leaks).expect("second write");

    let written = std::fs::read_to_string(&manifest).expect("read back");
    assert_eq!(
        written.matches("category = \"secrets\"").count(),
        4,
        "{written}"
    );
}

/// Two distinct credentials matched by the same rule on the same line are two
/// rows, because the identity includes the redacted preview.
#[test]
fn two_matches_on_one_line_stay_two_rows() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let manifest = manifest_at(tmp.path());
    let leaks = parse(
        r#"[{"File":"a.rs","StartLine":3,"Secret":"FIRST0SYNTHETIC0VALUE","RuleID":"x"},
            {"File":"a.rs","StartLine":3,"Secret":"SECOND0SYNTHETIC0VALUE","RuleID":"x"}]"#,
        Path::new("/nowhere"),
    )
    .expect("parses");

    write_into(&manifest, &leaks).expect("write");

    let written = std::fs::read_to_string(&manifest).expect("read back");
    assert_eq!(
        written.matches("category = \"secrets\"").count(),
        2,
        "{written}"
    );
}

/// An empty scan writes nothing at all, so the Assurance Scans section stays
/// absent rather than rendering an empty Secret Leakage table.
#[test]
fn an_empty_scan_writes_nothing_at_all() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let manifest = manifest_at(tmp.path());

    write_into(&manifest, &[]).expect("an empty write cannot fail");

    assert_manifest_unchanged(&manifest);
}

/// The manifest on disk still holds exactly what [`manifest_at`] wrote.
fn assert_manifest_unchanged(manifest: &Path) {
    let written = std::fs::read_to_string(manifest).expect("read back");
    assert_eq!(written, MANIFEST, "the manifest was modified");
}
