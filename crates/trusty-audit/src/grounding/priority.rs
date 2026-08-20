//! Writing the ranking, and the gaps, into the manifest (#6081, #6078).
//!
//! Why: the manifest is the interface (owner ruling 2026-08-19). trusty-review
//! reads `inspect_priority` off each `[[repositories]]` entry and inspects those
//! files ahead of its own path-name heuristics, and it reads `[report].gaps`
//! into the report's Gaps & Caveats section. A ranking this process holds and
//! does not write reaches no renderer — not `tga audit`'s, not
//! `crate::rerender`'s, and not the one the recipient runs over the delivered
//! package, which is the whole reason it goes in the file that ships.
//!
//! What: [`write_into`], a surgical `toml_edit` update of an existing manifest.
//! `toml_edit` rather than a `toml::Value` round trip because the file is
//! written by `tga` and read by `trusty-review`, and rewriting every key of a
//! document two other crates own — reordering it, dropping its comments — to add
//! one key is a much larger claim than this change is making.
//!
//! ## Which entry, and why by path
//!
//! The entry is matched on its `path`, never its `name`. The sweep generates
//! `tga`'s config with a filename-safe STEM as the repository name while the
//! operator sees the registered name, so matching on a name means agreeing with
//! a derivation this module cannot see. The checkout path is the same value on
//! both sides by construction — it is what was indexed.
//!
//! Test: `priority_tests`.

use std::path::Path;

use toml_edit::{Array, DocumentMut, Item, Table, Value};

/// Record `priorities` and `gaps` in the manifest at `path`.
///
/// Why gaps are written even when the ranking is not: a degraded leg is the one
/// thing that MUST reach the report. The two are independent — `[report].gaps`
/// belongs to the run, `inspect_priority` to one repository — so a gap is
/// recorded whether or not the repository entry can be found.
/// What: appends each gap to `[report].gaps`, skipping duplicates so a resumed
/// sweep does not restate them, and replaces the matched repository's
/// `inspect_priority` with the ranking. Nothing is written when there is nothing
/// to record.
///
/// # Errors
///
/// One line, safe to show the recipient, when the manifest cannot be read,
/// parsed, matched, or written back. The caller turns it into a gap of its own.
///
/// # Postconditions
/// On `Ok`, every line of `gaps` is in `[report].gaps` and — when `priorities`
/// is non-empty — the repository whose `path` is `checkout` declares exactly
/// those paths, in that order.
///
/// Test: `priority_tests::{the_ranking_lands_on_the_matching_repository,
/// gaps_are_appended_without_duplicating, a_ranking_with_no_matching_entry_is_refused}`.
pub fn write_into(
    path: &Path,
    checkout: &Path,
    priorities: &[String],
    gaps: &[String],
) -> Result<(), String> {
    if priorities.is_empty() && gaps.is_empty() {
        return Ok(());
    }
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("{} could not be read ({e})", path.display()))?;
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e| format!("{} is not readable as TOML ({e})", path.display()))?;

    record_gaps(&mut doc, gaps)?;
    if !priorities.is_empty() {
        record_priorities(&mut doc, checkout, priorities)?;
    }

    std::fs::write(path, doc.to_string())
        .map_err(|e| format!("{} could not be written ({e})", path.display()))
}

/// Append each gap to `[report].gaps`, creating the key when it is absent.
fn record_gaps(doc: &mut DocumentMut, gaps: &[String]) -> Result<(), String> {
    if gaps.is_empty() {
        return Ok(());
    }
    let report = doc
        .entry("report")
        .or_insert_with(|| Item::Table(Table::new()));
    let table = report
        .as_table_like_mut()
        .ok_or_else(|| "the manifest's `report` is not a table".to_string())?;
    let item = table
        .entry("gaps")
        .or_insert_with(|| Item::Value(Value::Array(Array::new())));
    let array = item
        .as_array_mut()
        .ok_or_else(|| "the manifest's `report.gaps` is not an array".to_string())?;
    for gap in gaps {
        if !array.iter().any(|v| v.as_str() == Some(gap.as_str())) {
            array.push(gap.as_str());
        }
    }
    Ok(())
}

/// Declare `priorities` on the `[[repositories]]` entry whose path is `checkout`.
fn record_priorities(
    doc: &mut DocumentMut,
    checkout: &Path,
    priorities: &[String],
) -> Result<(), String> {
    let repositories = doc
        .get_mut("repositories")
        .and_then(Item::as_array_of_tables_mut)
        .ok_or_else(|| "the manifest declares no `[[repositories]]` entry".to_string())?;
    let entry = repositories
        .iter_mut()
        .find(|table| names_checkout(table, checkout))
        .ok_or_else(|| {
            format!(
                "no `[[repositories]]` entry names the checkout at {}",
                checkout.display()
            )
        })?;
    entry.insert(
        "inspect_priority",
        Item::Value(Value::Array(ranked(priorities))),
    );
    Ok(())
}

/// Whether this repository entry's `path` is the checkout that was indexed.
///
/// Compared as written first, then through `canonicalize`, so a manifest naming
/// the same directory by a symlinked or non-normalised path still matches. A
/// path that cannot be canonicalised — the checkout has since been deleted —
/// falls back to the textual comparison rather than erroring.
fn names_checkout(entry: &Table, checkout: &Path) -> bool {
    let Some(declared) = entry.get("path").and_then(Item::as_str) else {
        return false;
    };
    let declared = Path::new(declared);
    if declared == checkout {
        return true;
    }
    match (declared.canonicalize(), checkout.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// The ranking as a multi-line TOML array of bare paths.
///
/// Bare strings rather than `{ path, weight }` tables on purpose: trusty-review
/// derives each entry's weight from its DECLARED POSITION (#6078's
/// `PRIORITY_BASE_WEIGHT` rule), so a rank expressed as order needs no agreement
/// about a numeric scale that lives in another crate and can move.
fn ranked(priorities: &[String]) -> Array {
    let mut array = Array::new();
    for path in priorities {
        let mut value = Value::from(path.as_str());
        value.decor_mut().set_prefix("\n    ");
        array.push_formatted(value);
    }
    array.set_trailing("\n");
    array.set_trailing_comma(true);
    array
}

#[cfg(test)]
mod priority_tests {
    use super::*;

    /// Shaped after what `tga::report::dd_manifest::DdManifest::to_toml` emits.
    const SAMPLE: &str = r#"[report]
title = "Acme — Technical Due Diligence"
gaps = ["Two repositories could not be cloned."]

[[repositories]]
name = "01-acme-api"
path = "/w/repos/acme-api"

[[repositories]]
name = "02-acme-web"
path = "/w/repos/acme-web"
"#;

    fn written(text: &str, checkout: &str, priorities: &[&str], gaps: &[&str]) -> String {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("manifest.toml");
        std::fs::write(&path, text).expect("write");
        let priorities: Vec<String> = priorities.iter().map(|s| (*s).to_owned()).collect();
        let gaps: Vec<String> = gaps.iter().map(|s| (*s).to_owned()).collect();
        write_into(&path, Path::new(checkout), &priorities, &gaps).expect("records");
        std::fs::read_to_string(&path).expect("read back")
    }

    /// The whole point: the ranking lands on the right repository, in order, and
    /// the OTHER repository is left exactly as it was.
    #[test]
    fn the_ranking_lands_on_the_matching_repository() {
        let out = written(
            SAMPLE,
            "/w/repos/acme-api",
            &["src/pay.rs", "src/auth.rs"],
            &[],
        );
        let parsed: toml::Value = toml::from_str(&out).expect("still valid TOML");
        let repos = parsed["repositories"].as_array().expect("array");
        assert_eq!(
            repos[0]["inspect_priority"]
                .as_array()
                .expect("declared")
                .iter()
                .map(|v| v.as_str().expect("string"))
                .collect::<Vec<_>>(),
            vec!["src/pay.rs", "src/auth.rs"],
        );
        assert!(
            repos[1].get("inspect_priority").is_none(),
            "the other repository must be untouched: {out}"
        );
    }

    /// The keys `tga` wrote and `trusty-review` reads survive the edit — the
    /// reason this is a `toml_edit` splice rather than a value round trip.
    #[test]
    fn everything_the_manifest_already_said_survives() {
        let out = written(SAMPLE, "/w/repos/acme-api", &["src/pay.rs"], &[]);
        assert!(
            out.contains("title = \"Acme — Technical Due Diligence\""),
            "{out}"
        );
        assert!(out.contains("name = \"02-acme-web\""), "{out}");
        assert!(
            out.contains("Two repositories could not be cloned."),
            "{out}"
        );
    }

    /// A degraded leg has to reach the report. It is appended, and a re-run over
    /// the same manifest does not say it twice.
    #[test]
    fn gaps_are_appended_without_duplicating() {
        let once = written(SAMPLE, "/w/repos/acme-api", &[], &["acme-api: no daemon"]);
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("manifest.toml");
        std::fs::write(&path, &once).expect("write");
        write_into(
            &path,
            Path::new("/w/repos/acme-api"),
            &[],
            &["acme-api: no daemon".to_owned()],
        )
        .expect("records");
        let twice = std::fs::read_to_string(&path).expect("read back");
        let parsed: toml::Value = toml::from_str(&twice).expect("valid TOML");
        let gaps = parsed["report"]["gaps"].as_array().expect("array");
        assert_eq!(gaps.len(), 2, "{twice}");
        assert_eq!(
            gaps.iter()
                .filter(|g| g.as_str() == Some("acme-api: no daemon"))
                .count(),
            1,
            "{twice}"
        );
    }

    /// A manifest with no `gaps` key gets one rather than losing the line.
    #[test]
    fn a_manifest_with_no_gaps_key_gains_one() {
        let out = written(
            "[report]\ntitle = \"Acme\"\n\n[[repositories]]\nname = \"a\"\npath = \"/w/a\"\n",
            "/w/a",
            &[],
            &["a: no daemon"],
        );
        let parsed: toml::Value = toml::from_str(&out).expect("valid TOML");
        assert_eq!(
            parsed["report"]["gaps"].as_array().expect("array")[0]
                .as_str()
                .expect("string"),
            "a: no daemon"
        );
    }

    /// Nothing to record must not rewrite a file two other crates own.
    #[test]
    fn nothing_to_record_writes_nothing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("manifest.toml");
        std::fs::write(&path, SAMPLE).expect("write");
        write_into(&path, Path::new("/w/repos/acme-api"), &[], &[]).expect("no-op");
        assert_eq!(std::fs::read_to_string(&path).expect("read back"), SAMPLE);
    }

    /// A ranking that cannot be attributed is refused rather than attached to
    /// whichever entry happened to be first — a report claiming trusty-analyze
    /// ranked a repository it never measured is worse than no ranking.
    #[test]
    fn a_ranking_with_no_matching_entry_is_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("manifest.toml");
        std::fs::write(&path, SAMPLE).expect("write");
        let err = write_into(
            &path,
            Path::new("/w/repos/nowhere"),
            &["src/a.rs".to_owned()],
            &[],
        )
        .expect_err("an unattributable ranking must be refused");
        assert!(err.contains("/w/repos/nowhere"), "{err}");
    }

    #[test]
    fn an_absent_manifest_is_a_reason_not_a_panic() {
        let err = write_into(
            Path::new("/nonexistent/manifest.toml"),
            Path::new("/w/a"),
            &["src/a.rs".to_owned()],
            &[],
        )
        .expect_err("an absent manifest must degrade");
        assert!(err.contains("could not be read"), "{err}");
    }

    #[test]
    fn a_manifest_that_is_not_toml_is_a_reason_not_a_panic() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("manifest.toml");
        std::fs::write(&path, "this is not toml = = =").expect("write");
        let err = write_into(&path, Path::new("/w/a"), &[], &["a: no daemon".to_owned()])
            .expect_err("a malformed manifest must degrade");
        assert!(err.contains("not readable as TOML"), "{err}");
    }
}
