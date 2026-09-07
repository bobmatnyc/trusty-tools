//! Tests for the collection-time citation check (#6791).
//!
//! Every arm is driven from a literal investigation snapshot and a real
//! checkout on disk, because the whole claim under test is what a filesystem
//! read says about a cited path. Nothing here spawns a process or touches a
//! daemon.

use std::fs;
use std::path::Path;

use tempfile::TempDir;

use super::{Citation, MANIFEST_KEY, Verdict, ground_into, judge, read_citations, write_into};

/// A snapshot with one repository, its findings, and optional trace anchors.
///
/// `findings` is `(title, file, line)`; `anchors` is `(title, file, symbol,
/// line)` and produces the `traces.traces[]` records #6166 writes.
fn snapshot(findings: &[(&str, &str, u64)], anchors: &[(&str, &str, &str, u64)]) -> String {
    let findings: Vec<String> = findings
        .iter()
        .map(|(title, file, line)| {
            format!(r#"{{"title":"{title}","file":"{file}","line":{line}}}"#)
        })
        .collect();
    let traces: Vec<String> = anchors
        .iter()
        .map(|(title, file, symbol, line)| {
            format!(
                r#"{{"title":"{title}","file":"{file}","anchor":{{"symbol":"{symbol}","file":"{file}","line":{line},"signature":""}}}}"#
            )
        })
        .collect();
    format!(
        r#"{{"repos":[{{"slug":"demo","name":"demo","findings":[{}],"traces":{{"traces":[{}]}}}}]}}"#,
        findings.join(","),
        traces.join(",")
    )
}

/// A manifest declaring one repository at `checkout`.
fn manifest_text(checkout: &Path) -> String {
    format!(
        "[report]\ntitle = \"demo\"\n\n[[repositories]]\nname = \"demo\"\npath = \"{}\"\n",
        checkout.display()
    )
}

/// A checkout holding `files`, each `(relative path, contents)`.
fn checkout_with(root: &Path, files: &[(&str, &str)]) {
    for (path, contents) in files {
        let full = root.join(path);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).expect("checkout directory");
        }
        fs::write(&full, contents).expect("checkout file");
    }
}

fn cite(file: &str, line: u64) -> Citation {
    Citation {
        title: "a finding".to_string(),
        file: file.to_string(),
        line: Some(line),
        symbol: None,
        symbol_line: None,
    }
}

/// The snapshot's cited findings become citations; an uncited GREEN topic does
/// not, because there is no path to check.
#[test]
fn the_snapshot_yields_one_citation_per_cited_finding() {
    let tmp = TempDir::new().expect("tempdir");
    let path = tmp.path().join("investigation.json");
    fs::write(
        &path,
        r#"{"repos":[{"findings":[
             {"title":"cited","file":"src/a.rs","line":7},
             {"title":"a strength","file":"","line":null}
           ]}]}"#,
    )
    .expect("snapshot");

    let citations = read_citations(&path).expect("reads");

    assert_eq!(citations.len(), 1, "{citations:?}");
    assert_eq!(citations[0].title, "cited");
    assert_eq!(citations[0].file, "src/a.rs");
    assert_eq!(citations[0].line, Some(7));
    assert_eq!(citations[0].symbol, None);
}

/// A trace record that reached an anchor supplies the symbol and the line it
/// was anchored at; one that did not supplies neither.
#[test]
fn an_anchor_supplies_the_symbol_and_its_line() {
    let tmp = TempDir::new().expect("tempdir");
    let path = tmp.path().join("investigation.json");
    fs::write(
        &path,
        snapshot(
            &[("anchored", "src/a.rs", 7), ("refused", "src/b.rs", 3)],
            &[("anchored", "src/a.rs", "Guard::check", 5)],
        ),
    )
    .expect("snapshot");

    let citations = read_citations(&path).expect("reads");

    let anchored = &citations[0];
    assert_eq!(anchored.symbol.as_deref(), Some("Guard::check"));
    assert_eq!(anchored.symbol_line, Some(5));
    let refused = &citations[1];
    assert_eq!(refused.symbol, None, "{refused:?}");
    assert_eq!(refused.symbol_line, None);
}

/// A snapshot that is not there at all is a read error the caller can report.
#[test]
fn a_missing_snapshot_is_a_named_gap() {
    let tmp = TempDir::new().expect("tempdir");

    let cause =
        read_citations(&tmp.path().join("investigation.json")).expect_err("there is no snapshot");

    assert!(cause.contains("citation-check"), "{cause}");
    assert!(cause.contains("could not be read"), "{cause}");
}

/// The core claim: a citation whose file and line are still on disk is
/// CONFIRMED, and it is confirmed by reading the checkout rather than by asking
/// a model.
#[test]
fn a_present_citation_is_confirmed() {
    let tmp = TempDir::new().expect("tempdir");
    checkout_with(tmp.path(), &[("src/a.rs", "one\ntwo\nthree\n")]);

    let verdicts = judge(tmp.path(), &[cite("src/a.rs", 2)]);

    assert_eq!(verdicts[0].verdict, Verdict::Confirmed, "{verdicts:?}");
    assert!(verdicts[0].reason.is_empty(), "{verdicts:?}");
}

/// A cited path the checkout no longer carries is STALE, and the reason names
/// the path.
#[test]
fn a_deleted_file_is_stale() {
    let tmp = TempDir::new().expect("tempdir");
    checkout_with(tmp.path(), &[("src/a.rs", "one\n")]);

    let verdicts = judge(tmp.path(), &[cite("src/gone.rs", 1)]);

    assert_eq!(verdicts[0].verdict, Verdict::Stale, "{verdicts:?}");
    assert!(verdicts[0].reason.contains("src/gone.rs"), "{verdicts:?}");
}

/// A file that shrank past the cited line is STALE, and the reason states the
/// length it now has.
#[test]
fn a_line_past_the_end_of_the_file_is_stale() {
    let tmp = TempDir::new().expect("tempdir");
    checkout_with(tmp.path(), &[("src/a.rs", "one\ntwo\n")]);

    let verdicts = judge(tmp.path(), &[cite("src/a.rs", 9)]);

    assert_eq!(verdicts[0].verdict, Verdict::Stale, "{verdicts:?}");
    assert!(verdicts[0].reason.contains("2 line(s)"), "{verdicts:?}");
}

/// A traced symbol still on its anchored line confirms; one that has moved off
/// it is STALE naming the symbol.
#[test]
fn a_symbol_that_left_its_anchored_line_is_stale() {
    let tmp = TempDir::new().expect("tempdir");
    checkout_with(
        tmp.path(),
        &[("src/a.rs", "// header\nfn check() {}\nfn other() {}\n")],
    );
    let anchored = |line| Citation {
        title: "a finding".to_string(),
        file: "src/a.rs".to_string(),
        line: Some(2),
        symbol: Some("Guard::check".to_string()),
        symbol_line: Some(line),
    };

    let verdicts = judge(tmp.path(), &[anchored(2), anchored(3)]);

    assert_eq!(verdicts[0].verdict, Verdict::Confirmed, "{verdicts:?}");
    assert_eq!(verdicts[1].verdict, Verdict::Stale, "{verdicts:?}");
    assert!(verdicts[1].reason.contains("Guard::check"), "{verdicts:?}");
}

/// A missing checkout is UNREACHABLE for every citation, never STALE. This is
/// the 0.13.2 outcome the issue names: a render with no repository must report
/// unassessed rather than failed.
#[test]
fn a_missing_checkout_is_unreachable_not_stale() {
    let tmp = TempDir::new().expect("tempdir");
    let absent = tmp.path().join("no-such-checkout");

    let verdicts = judge(&absent, &[cite("src/a.rs", 1), cite("src/b.rs", 2)]);

    assert_eq!(verdicts.len(), 2, "{verdicts:?}");
    assert!(
        verdicts.iter().all(|v| v.verdict == Verdict::Unreachable),
        "{verdicts:?}"
    );
    assert!(verdicts[0].reason.contains("not present"), "{verdicts:?}");
}

/// A cited path that climbs out of the checkout is never CONFIRMED, however
/// readable the file it names is.
#[test]
fn a_citation_escaping_the_checkout_is_never_confirmed() {
    let tmp = TempDir::new().expect("tempdir");
    let checkout = tmp.path().join("repo");
    checkout_with(&checkout, &[("src/a.rs", "one\n")]);
    fs::write(tmp.path().join("outside.rs"), "one\n").expect("outside file");

    let verdicts = judge(&checkout, &[cite("../outside.rs", 1)]);

    assert_eq!(verdicts[0].verdict, Verdict::Stale, "{verdicts:?}");
    assert!(
        verdicts[0].reason.contains("not inside the checkout"),
        "{verdicts:?}"
    );
}

/// The verdicts reach the manifest — the artifact that ships in the bundle —
/// on the repository entry that names this checkout.
#[test]
fn the_verdicts_land_on_the_matching_repository() {
    let tmp = TempDir::new().expect("tempdir");
    let checkout = tmp.path().join("repo");
    checkout_with(&checkout, &[("src/a.rs", "one\ntwo\n")]);
    let manifest = tmp.path().join("manifest.toml");
    fs::write(&manifest, manifest_text(&checkout)).expect("manifest");

    let verdicts = judge(&checkout, &[cite("src/a.rs", 1), cite("src/gone.rs", 1)]);
    write_into(&manifest, &checkout, &verdicts).expect("writes");

    let written = fs::read_to_string(&manifest).expect("reads back");
    assert!(written.contains(MANIFEST_KEY), "{written}");
    assert!(written.contains(r#"verdict = "confirmed""#), "{written}");
    assert!(written.contains(r#"verdict = "stale""#), "{written}");
    // The document's own keys survive the format-preserving write.
    assert!(written.contains(r#"title = "demo""#), "{written}");
}

/// A resumed sweep restates the verdicts rather than appending a second copy.
#[test]
fn a_second_run_restates_rather_than_duplicates() {
    let tmp = TempDir::new().expect("tempdir");
    let checkout = tmp.path().join("repo");
    checkout_with(&checkout, &[("src/a.rs", "one\n")]);
    let manifest = tmp.path().join("manifest.toml");
    fs::write(&manifest, manifest_text(&checkout)).expect("manifest");
    let verdicts = judge(&checkout, &[cite("src/a.rs", 1)]);

    write_into(&manifest, &checkout, &verdicts).expect("first write");
    write_into(&manifest, &checkout, &verdicts).expect("second write");

    let written = fs::read_to_string(&manifest).expect("reads back");
    assert_eq!(
        written.matches(r#"verdict = "confirmed""#).count(),
        1,
        "{written}"
    );
}

/// The leg runs end to end at collection time: snapshot beside the manifest,
/// checkout on disk, verdicts in the manifest, and the stale count stated as a
/// gap.
#[test]
fn stale_citations_are_counted_in_a_gap_line() {
    let tmp = TempDir::new().expect("tempdir");
    let checkout = tmp.path().join("repo");
    checkout_with(&checkout, &[("src/a.rs", "one\ntwo\n")]);
    let out = tmp.path().join("out");
    fs::create_dir_all(&out).expect("output directory");
    let manifest = out.join("manifest.toml");
    fs::write(&manifest, manifest_text(&checkout)).expect("manifest");
    fs::write(
        out.join("investigation.json"),
        snapshot(&[("live", "src/a.rs", 1), ("moved", "src/gone.rs", 4)], &[]),
    )
    .expect("snapshot");

    let gaps = ground_into(&manifest, &checkout, "demo");

    assert_eq!(gaps.len(), 1, "{gaps:?}");
    assert!(gaps[0].contains("1 of 2 finding citation(s)"), "{gaps:?}");
    let written = fs::read_to_string(&manifest).expect("reads back");
    assert!(written.contains(r#"verdict = "confirmed""#), "{written}");
    assert!(written.contains(r#"verdict = "stale""#), "{written}");
}

/// No snapshot beside the manifest means the render has not happened yet.
/// Nothing was lost, so the leg writes nothing and says nothing.
#[test]
fn a_render_that_has_not_happened_yet_is_silent() {
    let tmp = TempDir::new().expect("tempdir");
    let checkout = tmp.path().join("repo");
    checkout_with(&checkout, &[("src/a.rs", "one\n")]);
    let manifest = tmp.path().join("manifest.toml");
    fs::write(&manifest, manifest_text(&checkout)).expect("manifest");

    let gaps = ground_into(&manifest, &checkout, "demo");

    assert!(gaps.is_empty(), "{gaps:?}");
    let written = fs::read_to_string(&manifest).expect("reads back");
    assert!(!written.contains(MANIFEST_KEY), "{written}");
}

/// A snapshot that exists and cannot be read IS a gap, and the line says what
/// a later render loses by it.
#[test]
fn an_unreadable_snapshot_is_a_named_gap() {
    let tmp = TempDir::new().expect("tempdir");
    let checkout = tmp.path().join("repo");
    checkout_with(&checkout, &[("src/a.rs", "one\n")]);
    let manifest = tmp.path().join("manifest.toml");
    fs::write(&manifest, manifest_text(&checkout)).expect("manifest");
    fs::write(tmp.path().join("investigation.json"), "{not json").expect("snapshot");

    let gaps = ground_into(&manifest, &checkout, "demo");

    assert_eq!(gaps.len(), 1, "{gaps:?}");
    assert!(gaps[0].contains("not readable as JSON"), "{gaps:?}");
    assert!(gaps[0].contains("moved past"), "{gaps:?}");
}
