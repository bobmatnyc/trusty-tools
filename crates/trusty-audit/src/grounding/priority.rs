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

use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, Value};

/// One ranked path, and what put it there (#6082).
///
/// Why: a ranking that only says WHICH files to read cannot tell the report why
/// any of them was read, and "why" is the whole difference between a coverage
/// section that states a percentage and one a due-diligence reader can weigh.
/// The two optional fields are what trusty-review renders per dimension.
/// What: `path` is repo-relative; `dimension` is a DD dimension spelled as
/// trusty-review spells it (absent for a signal that belongs to no single
/// dimension, such as a complexity hotspot); `reason` is one line naming the
/// query or measurement that selected it.
/// Test: `priority_tests::a_dimension_and_reason_are_written_as_a_table`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct Priority {
    /// Repo-relative path to inspect.
    pub path: String,
    /// The DD dimension this file is evidence for, when it is evidence for one.
    pub dimension: Option<String>,
    /// One line naming the query or measurement that selected it.
    pub reason: Option<String>,
}

impl Priority {
    /// A ranked path with no attribution — the shape #6081 wrote.
    #[must_use]
    pub fn bare(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            dimension: None,
            reason: None,
        }
    }
}

/// The investigation budget the audit asks for, in files and content bytes.
///
/// Why: trusty-review's own defaults (40 files / 400 KiB) read about 1% of a
/// workspace-sized repository, which the owner ruled too shallow for a DD
/// report (#6082). The budget is a manifest key, so raising it is one more
/// thing written to the interface rather than a flag the operator must learn —
/// `trusty-audit audit` still takes no new step.
/// What: written to `[report]` PER KEY, and only where the manifest declares
/// none — an operator who set one of the two keeps it and gets the audit's
/// default for the other, rather than falling back to trusty-review's.
/// Test: `priority_tests::{the_budget_is_recorded_once,
/// a_declared_budget_is_left_alone}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Budget {
    /// Files the investigation pass may read.
    pub max_files: usize,
    /// Total content bytes it may send.
    pub max_bytes: usize,
}

/// Files the audit asks the investigation pass to read per repository.
pub const DEFAULT_MAX_FILES: usize = 120;

/// Content bytes the audit asks it to send per repository.
pub const DEFAULT_MAX_BYTES: usize = 1_200 * 1024;

/// Environment override for [`DEFAULT_MAX_FILES`].
pub const ENV_MAX_FILES: &str = "TRUSTY_AUDIT_INVESTIGATE_MAX_FILES";

/// Environment override for [`DEFAULT_MAX_BYTES`].
pub const ENV_MAX_BYTES: &str = "TRUSTY_AUDIT_INVESTIGATE_MAX_BYTES";

impl Budget {
    /// The budget this machine asks for, defaults unless overridden.
    #[must_use]
    pub fn from_env() -> Self {
        Self::resolved(
            std::env::var(ENV_MAX_FILES).ok().as_deref(),
            std::env::var(ENV_MAX_BYTES).ok().as_deref(),
        )
    }

    /// [`Budget::from_env`]'s rule, as a pure function.
    ///
    /// Why: reading the two variables is all `from_env` does, so the RULE —
    /// an unparseable or zero override falls back rather than disabling the
    /// investigation — is testable without `std::env::set_var`, which is
    /// `unsafe` in edition 2024 and unsound under the parallel harness.
    /// What: a positive integer wins; anything else takes the default.
    /// Test: `priority_tests::an_unusable_override_falls_back_to_the_default`.
    #[must_use]
    pub fn resolved(files: Option<&str>, bytes: Option<&str>) -> Self {
        Self {
            max_files: positive(files).unwrap_or(DEFAULT_MAX_FILES),
            max_bytes: positive(bytes).unwrap_or(DEFAULT_MAX_BYTES),
        }
    }
}

/// A declared override, when it parses as a positive count.
fn positive(value: Option<&str>) -> Option<usize> {
    value?.trim().parse::<usize>().ok().filter(|n| *n > 0)
}

/// Record `priorities`, `budget` and `gaps` in the manifest at `path`.
///
/// Why gaps are written even when the ranking is not: a degraded leg is the one
/// thing that MUST reach the report. The two are independent — `[report].gaps`
/// belongs to the run, `inspect_priority` to one repository — so a gap is
/// recorded whether or not the repository entry can be found.
/// What: appends each gap to `[report].gaps`, skipping duplicates so a resumed
/// sweep does not restate them, fills the investigation budget keys when the
/// manifest declares none, and replaces the matched repository's
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
    priorities: &[Priority],
    budget: Option<Budget>,
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
        if let Some(budget) = budget {
            record_budget(&mut doc, budget)?;
        }
    }

    std::fs::write(path, doc.to_string())
        .map_err(|e| format!("{} could not be written ({e})", path.display()))
}

/// Fill `[report]`'s investigation budget keys, leaving declared ones alone.
fn record_budget(doc: &mut DocumentMut, budget: Budget) -> Result<(), String> {
    let report = doc
        .entry("report")
        .or_insert_with(|| Item::Table(Table::new()));
    let table = report
        .as_table_like_mut()
        .ok_or_else(|| "the manifest's `report` is not a table".to_string())?;
    for (key, value) in [
        ("investigate_max_files", budget.max_files),
        ("investigate_max_bytes", budget.max_bytes),
    ] {
        if table.get(key).is_none() {
            table.insert(
                key,
                Item::Value(Value::from(i64::try_from(value).unwrap_or(i64::MAX))),
            );
        }
    }
    Ok(())
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
    priorities: &[Priority],
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

/// The ranking as a multi-line TOML array.
///
/// No `weight` key on purpose: trusty-review derives each entry's weight from
/// its DECLARED POSITION (#6078's `PRIORITY_BASE_WEIGHT` rule), so a rank
/// expressed as order needs no agreement about a numeric scale that lives in
/// another crate and can move. An entry with no attribution stays a bare string
/// — the #6081 shape, and still what trusty-review reads for a hand-written
/// manifest.
fn ranked(priorities: &[Priority]) -> Array {
    let mut array = Array::new();
    for priority in priorities {
        let mut value = entry(priority);
        value.decor_mut().set_prefix("\n    ");
        array.push_formatted(value);
    }
    array.set_trailing("\n");
    array.set_trailing_comma(true);
    array
}

/// One priority as TOML: a bare path, or a table when it carries attribution.
fn entry(priority: &Priority) -> Value {
    if priority.dimension.is_none() && priority.reason.is_none() {
        return Value::from(priority.path.as_str());
    }
    let mut table = InlineTable::new();
    table.insert("path", Value::from(priority.path.as_str()));
    if let Some(dimension) = &priority.dimension {
        table.insert("dimension", Value::from(dimension.as_str()));
    }
    if let Some(reason) = &priority.reason {
        table.insert("reason", Value::from(reason.as_str()));
    }
    Value::InlineTable(table)
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
        let priorities: Vec<Priority> = priorities.iter().map(|s| Priority::bare(*s)).collect();
        recorded(text, checkout, &priorities, None, gaps)
    }

    fn recorded(
        text: &str,
        checkout: &str,
        priorities: &[Priority],
        budget: Option<Budget>,
        gaps: &[&str],
    ) -> String {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("manifest.toml");
        std::fs::write(&path, text).expect("write");
        let gaps: Vec<String> = gaps.iter().map(|s| (*s).to_owned()).collect();
        write_into(&path, Path::new(checkout), priorities, budget, &gaps).expect("records");
        std::fs::read_to_string(&path).expect("read back")
    }

    fn attributed(path: &str, dimension: &str, reason: &str) -> Priority {
        Priority {
            path: path.to_owned(),
            dimension: Some(dimension.to_owned()),
            reason: Some(reason.to_owned()),
        }
    }

    /// #6082: an attributed entry becomes a table carrying its dimension and the
    /// query that found it — the "why this file" the coverage section renders.
    #[test]
    fn a_dimension_and_reason_are_written_as_a_table() {
        let out = recorded(
            SAMPLE,
            "/w/repos/acme-api",
            &[attributed(
                "src/auth.rs",
                "authentication & secrets",
                "trusty-search hit for \"credential handling\" (score 0.82, line 40)",
            )],
            None,
            &[],
        );
        let parsed: toml::Value = toml::from_str(&out).expect("still valid TOML");
        let first = &parsed["repositories"].as_array().expect("array")[0]["inspect_priority"]
            .as_array()
            .expect("declared")[0];
        assert_eq!(first["path"].as_str(), Some("src/auth.rs"));
        assert_eq!(
            first["dimension"].as_str(),
            Some("authentication & secrets")
        );
        assert!(
            first["reason"]
                .as_str()
                .expect("reason")
                .contains("credential handling"),
            "{out}"
        );
    }

    /// An override that cannot be used must not turn the investigation off.
    #[test]
    fn an_unusable_override_falls_back_to_the_default() {
        assert_eq!(
            Budget::resolved(Some("0"), Some("not a number")),
            Budget {
                max_files: DEFAULT_MAX_FILES,
                max_bytes: DEFAULT_MAX_BYTES,
            }
        );
        assert_eq!(Budget::resolved(Some(" 12 "), None).max_files, 12);
    }

    /// #6082: the budget the audit asks for is recorded once, and only when the
    /// manifest is silent about it.
    #[test]
    fn the_budget_is_recorded_once() {
        let out = recorded(
            SAMPLE,
            "/w/repos/acme-api",
            &[Priority::bare("src/pay.rs")],
            Some(Budget {
                max_files: 120,
                max_bytes: 1_200_000,
            }),
            &[],
        );
        let parsed: toml::Value = toml::from_str(&out).expect("valid TOML");
        assert_eq!(
            parsed["report"]["investigate_max_files"].as_integer(),
            Some(120)
        );
        assert_eq!(
            parsed["report"]["investigate_max_bytes"].as_integer(),
            Some(1_200_000)
        );
    }

    /// An operator who declared a budget keeps it — the audit fills, never
    /// overrides — and it fills PER KEY, so declaring one of the two leaves the
    /// other at the audit's default rather than at trusty-review's.
    #[test]
    fn a_declared_budget_is_left_alone() {
        for (declared_key, declared_value) in [
            ("investigate_max_files", 7_i64),
            ("investigate_max_bytes", 4096),
        ] {
            let manifest = SAMPLE.replace(
                "title = \"Acme — Technical Due Diligence\"",
                &format!("title = \"Acme\"\n{declared_key} = {declared_value}"),
            );
            let out = recorded(
                &manifest,
                "/w/repos/acme-api",
                &[Priority::bare("src/pay.rs")],
                Some(Budget {
                    max_files: 120,
                    max_bytes: 1_200_000,
                }),
                &[],
            );
            let parsed: toml::Value = toml::from_str(&out).expect("valid TOML");
            assert_eq!(
                parsed["report"][declared_key].as_integer(),
                Some(declared_value),
                "the declared key survives: {out}"
            );
            let filled = if declared_key == "investigate_max_files" {
                ("investigate_max_bytes", 1_200_000)
            } else {
                ("investigate_max_files", 120)
            };
            assert_eq!(
                parsed["report"][filled.0].as_integer(),
                Some(filled.1),
                "the undeclared key still gets the audit's default: {out}"
            );
        }
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
            None,
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
        write_into(&path, Path::new("/w/repos/acme-api"), &[], None, &[]).expect("no-op");
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
            &[Priority::bare("src/a.rs")],
            None,
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
            &[Priority::bare("src/a.rs")],
            None,
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
        let err = write_into(
            &path,
            Path::new("/w/a"),
            &[],
            None,
            &["a: no daemon".to_owned()],
        )
        .expect_err("a malformed manifest must degrade");
        assert!(err.contains("not readable as TOML"), "{err}");
    }
}
