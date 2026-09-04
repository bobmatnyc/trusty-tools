//! The bundle-level technical-debt roll-up, and the `report.json` carrying it (#6781).
//!
//! Why: every assurance collector writes its rows into one repository's
//! `[report].findings` (`crate::grounding::findings`), and nothing ever added
//! them up. A consumer wanting "how many RED findings does this engagement
//! have" had to open each repository's `manifest.toml`, parse the array, and
//! count — once per consumer, each with its own idea of what an empty band or
//! an unrecognised one means. Two consumers counting the same array their own
//! way is how two numbers describing one engagement start to disagree.
//!
//! What: [`DebtRollup`], the counts by tier, by dimension, by repository, and by
//! tier × dimension, plus their shared [`DebtRollup::total`]. It is computed
//! ONCE per run — [`from_manifests`] reads each repository's manifest — and both
//! consumers read that one value: `crate::index_report` renders its "Technical
//! debt by tier" table from it, and [`write()`] serialises it to `report.json`
//! beside the index.
//!
//! ## The taxonomy is the producer's, not this module's
//!
//! - **tier** is a finding's `severity` — the RED / AMBER band vocabulary
//!   `crate::grounding::cve::Severity` spells and the other three collectors
//!   reuse, and the same vocabulary trusty-review's Assurance Scans table
//!   renders verbatim. Nothing is invented here and nothing is re-banded.
//! - **dimension** is a finding's `category` — `dependencies`, `license`,
//!   `secrets`, `churn`, the four `CATEGORY` constants under
//!   `crate::grounding`.
//!
//! A band or a category this crate does not recognise is counted under its own
//! name rather than dropped, for the reason trusty-review renders one rather
//! than erroring: the producer owns the vocabulary, and a collector added after
//! this module was written must still reach the total.
//!
//! ## Fail-open reading
//!
//! A repository whose manifest is absent, unreadable, or not parseable
//! contributes nothing and raises nothing. The index already states why a unit
//! produced no report; a roll-up that refused to render because one repository
//! failed would lose the counts for the ones that succeeded.
//!
//! Test: `debt_rollup_tests`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::AuditError;

/// The bundle-level machine-readable report, beside `index.md`.
///
/// `report.json` is unclaimed at bundle level: `tga` writes one per repository,
/// inside that repository's own directory, so the two never collide.
pub const REPORT_FILE: &str = "report.json";

/// The tier a finding is counted under when its producer left `severity` empty.
///
/// Counted rather than dropped: a finding with no band is still a finding, and
/// silently discarding it would make the marginals disagree with the row count.
pub const UNSPECIFIED_TIER: &str = "UNSPECIFIED";

/// The dimension a finding is counted under when its producer left `category`
/// empty — the same reading trusty-review's Assurance Scans section gives it.
pub const UNCATEGORISED_DIMENSION: &str = "uncategorised";

/// Counts of every declared finding, by tier, by dimension, and by repository.
///
/// Why: the one place an engagement's finding totals are derived, so a renderer
/// and a JSON consumer cannot report different numbers for the same run.
/// What: four cross-tabulations over the same population plus [`Self::total`],
/// the population's size. Every marginal sums to `total` — `by_tier`,
/// `by_dimension`, each repository's map summed over repositories, and
/// `by_tier_dimension` summed over both axes. `BTreeMap` throughout, so the
/// serialised key order is deterministic across runs.
/// Test: `debt_rollup_tests::every_marginal_sums_to_the_total`,
/// `debt_rollup_tests::the_json_carries_the_rollup_block`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct DebtRollup {
    /// Findings per tier, e.g. `{"RED": 3, "AMBER": 11}`.
    pub by_tier: BTreeMap<String, usize>,
    /// Findings per dimension, e.g. `{"dependencies": 9, "secrets": 5}`.
    pub by_dimension: BTreeMap<String, usize>,
    /// Findings per repository, then per tier within it.
    pub by_repo: BTreeMap<String, BTreeMap<String, usize>>,
    /// Findings per tier, then per dimension within it.
    pub by_tier_dimension: BTreeMap<String, BTreeMap<String, usize>>,
    /// Every finding counted, across every repository.
    pub total: usize,
}

impl DebtRollup {
    /// Count one finding into every cross-tabulation at once.
    ///
    /// Why: the four maps and the total are one fact seen four ways. Updating
    /// them from four call sites is how a marginal drifts from the total; this
    /// is the only writer, so the invariant holds by construction.
    /// What: normalises `tier` (trimmed, upper-cased, empty →
    /// [`UNSPECIFIED_TIER`]) and `dimension` (trimmed, empty →
    /// [`UNCATEGORISED_DIMENSION`]), then increments five counters.
    ///
    /// # Postconditions
    /// `total` grows by exactly one, and so does exactly one bucket in each of
    /// the four maps.
    ///
    /// Test: `debt_rollup_tests::every_marginal_sums_to_the_total`,
    /// `debt_rollup_tests::an_unbanded_finding_is_counted_not_dropped`.
    pub fn tally(&mut self, repo: &str, tier: &str, dimension: &str) {
        let tier = normalised_tier(tier);
        let dimension = normalised_dimension(dimension);
        *self.by_tier.entry(tier.clone()).or_default() += 1;
        *self.by_dimension.entry(dimension.clone()).or_default() += 1;
        *self
            .by_repo
            .entry(repo.trim().to_owned())
            .or_default()
            .entry(tier.clone())
            .or_default() += 1;
        *self
            .by_tier_dimension
            .entry(tier)
            .or_default()
            .entry(dimension)
            .or_default() += 1;
        self.total += 1;
    }

    /// Whether this run declared no findings at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.total == 0
    }

    /// The tiers present, worst band first and then alphabetically.
    ///
    /// The order trusty-review's Assurance Scans table sorts its rows in — RED,
    /// then AMBER, then every band neither crate recognises — so a reader
    /// meeting both documents meets one ordering.
    /// Test: `debt_rollup_tests::the_table_orders_red_before_amber`.
    #[must_use]
    pub fn tiers(&self) -> Vec<&str> {
        let mut tiers: Vec<&str> = self.by_tier.keys().map(String::as_str).collect();
        tiers.sort_by_key(|tier| (band_rank(tier), *tier));
        tiers
    }

    /// The "Technical debt by tier" section, or `""` when nothing was found.
    ///
    /// Why: the index's one cross-repository view of the findings. It reads
    /// every cell out of this value — including the grand total, which is
    /// [`Self::total`] rather than a second sum over the rows — so the table and
    /// `report.json` cannot state different numbers.
    /// What: one row per repository, one column per tier present, plus a totals
    /// row taken from [`Self::by_tier`]. An empty roll-up renders nothing at
    /// all, for the reason trusty-review's Assurance Scans section does: a
    /// heading over an empty table reads as a scan that found nothing, which is
    /// a claim no collector made.
    /// Test: `debt_rollup_tests::the_table_reads_its_totals_from_the_rollup`,
    /// `debt_rollup_tests::an_empty_rollup_renders_nothing`.
    #[must_use]
    pub fn table(&self) -> String {
        if self.is_empty() {
            return String::new();
        }
        let tiers = self.tiers();
        let mut out = String::from("## Technical debt by tier\n\n");
        out.push_str(
            "Counted once from every repository's `[report].findings` and written to \
             `report.json` beside this file, which also carries the per-dimension and \
             tier-by-dimension breakdowns. The bands are the collectors' own; nothing here \
             re-bands a finding.\n\n",
        );
        out.push_str("| repository |");
        for tier in &tiers {
            out.push_str(&format!(" {tier} |"));
        }
        out.push_str(" total |\n| --- |");
        for _ in &tiers {
            out.push_str(" --- |");
        }
        out.push_str(" --- |\n");
        for (repo, counts) in &self.by_repo {
            out.push_str(&format!("| {repo} |"));
            let mut row_total = 0;
            for tier in &tiers {
                let count = counts.get(*tier).copied().unwrap_or(0);
                row_total += count;
                out.push_str(&format!(" {count} |"));
            }
            out.push_str(&format!(" {row_total} |\n"));
        }
        out.push_str("| **all repositories** |");
        for tier in &tiers {
            out.push_str(&format!(
                " **{}** |",
                self.by_tier.get(*tier).copied().unwrap_or(0)
            ));
        }
        out.push_str(&format!(" **{}** |\n\n", self.total));
        out
    }
}

/// Sort key: RED before AMBER before every band this crate does not recognise.
///
/// `crate::grounding::cve::Severity` spells only the first two; the third arm
/// is what keeps a band a later collector introduces sorting last rather than
/// panicking.
fn band_rank(tier: &str) -> u8 {
    match tier {
        "RED" => 0,
        "AMBER" => 1,
        "GREEN" => 2,
        _ => 3,
    }
}

/// A finding's `severity`, trimmed and upper-cased, or [`UNSPECIFIED_TIER`].
///
/// Upper-casing matches how trusty-review's table ranks a band, so `Red` and
/// `RED` from two collectors land in one bucket rather than two.
fn normalised_tier(tier: &str) -> String {
    let trimmed = tier.trim();
    if trimmed.is_empty() {
        return UNSPECIFIED_TIER.to_owned();
    }
    trimmed.to_ascii_uppercase()
}

/// A finding's `category`, trimmed, or [`UNCATEGORISED_DIMENSION`].
///
/// Trimmed but NOT case-folded: the four `CATEGORY` constants are lower-case
/// literals, and case-folding a dimension would silently rename a collector's
/// own label in the delivered JSON.
fn normalised_dimension(dimension: &str) -> String {
    let trimmed = dimension.trim();
    if trimmed.is_empty() {
        return UNCATEGORISED_DIMENSION.to_owned();
    }
    trimmed.to_owned()
}

/// Just enough of a manifest to read its findings, ignoring everything else.
///
/// Deliberately not `crate::manifest::AuditManifest`: that type models what
/// this crate WRITES, and the findings array is a third party's — a collector
/// adding a key (a CWE tag, an OSV identifier) must not make this reader fail.
/// Every unknown key and every unknown section is dropped by serde.
#[derive(Debug, Default, Deserialize)]
struct FindingsDoc {
    #[serde(default)]
    report: FindingsBlock,
}

/// The `[report]` table's findings array, and nothing else from it.
#[derive(Debug, Default, Deserialize)]
struct FindingsBlock {
    #[serde(default)]
    findings: Vec<FindingRow>,
}

/// The two columns the roll-up counts on, out of a row that carries more.
#[derive(Debug, Default, Deserialize)]
struct FindingRow {
    /// The producer's band — the roll-up's tier.
    #[serde(default)]
    severity: String,
    /// The collector that produced it — the roll-up's dimension.
    #[serde(default)]
    category: String,
}

/// The `(tier, dimension)` of every finding one manifest declares.
///
/// Why: the manifest is the interface (owner ruling 2026-08-19). The roll-up
/// reads what the collectors wrote rather than re-running any scan, so it
/// describes the delivered artifact exactly.
/// What: fail-open — an absent, unreadable, or unparseable manifest yields an
/// empty list, because the index already states why that unit has no report and
/// a roll-up that refused over one repository would lose the other repositories'
/// counts.
/// Test: `debt_rollup_tests::a_manifest_yields_its_findings`,
/// `debt_rollup_tests::an_unreadable_manifest_contributes_nothing`.
#[must_use]
pub fn read_findings(manifest: &Path) -> Vec<(String, String)> {
    let Ok(text) = std::fs::read_to_string(manifest) else {
        return Vec::new();
    };
    let Ok(doc) = toml::from_str::<FindingsDoc>(&text) else {
        return Vec::new();
    };
    doc.report
        .findings
        .into_iter()
        .map(|row| (row.severity, row.category))
        .collect()
}

/// Roll every unit's manifest up into one [`DebtRollup`].
///
/// Why: the ONE computation of these counts per run. Both consumers — the
/// index's table and `report.json` — are handed its result rather than counting
/// the findings again for themselves.
/// What: takes `(repository name, manifest path)` pairs in the run's own order
/// and reads each manifest exactly once. A repository that declares no findings
/// is absent from [`DebtRollup::by_repo`] rather than carrying an all-zero row,
/// so the table names only the repositories that contributed a finding.
/// Test: `debt_rollup_tests::three_repositories_roll_up_into_one_block`.
#[must_use]
pub fn from_manifests<I>(units: I) -> DebtRollup
where
    I: IntoIterator<Item = (String, PathBuf)>,
{
    let mut rollup = DebtRollup::default();
    for (repo, manifest) in units {
        for (tier, dimension) in read_findings(&manifest) {
            rollup.tally(&repo, &tier, &dimension);
        }
    }
    rollup
}

/// The bundle-level `report.json` document.
///
/// Why: a named block rather than a bare roll-up at the document root, so a
/// later run-level fact can join it without moving the counts a consumer has
/// already wired to.
/// What: the run's local timestamp — the same string the index states — and the
/// `debt_rollup` block itself.
/// Test: `debt_rollup_tests::the_json_carries_the_rollup_block`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct BundleReport<'a> {
    /// Local time with its UTC offset, from `crate::index_report::local_now`.
    pub generated_at: &'a str,
    /// The counts, by tier, by dimension, by repository, and by tier × dimension.
    pub debt_rollup: &'a DebtRollup,
}

/// The bundle report's JSON, pretty-printed.
///
/// Serialisation of `BTreeMap`s and `usize`s cannot fail, so the fallible arm is
/// unreachable; it still yields an empty document rather than panicking, because
/// a run that has just written every report must not die on its index.
/// Test: `debt_rollup_tests::the_json_carries_the_rollup_block`.
#[must_use]
pub fn to_json(rollup: &DebtRollup, generated_at: &str) -> String {
    let document = BundleReport {
        generated_at,
        debt_rollup: rollup,
    };
    serde_json::to_string_pretty(&document).unwrap_or_else(|_| "{}".to_owned())
}

/// Write `report.json` into `dir`, replacing any earlier one.
///
/// # Postconditions
/// On `Ok`, `dir/report.json` carries this run's `debt_rollup` block. Nothing
/// outside `dir` is written, which is what keeps `crate::rerender`'s "the source
/// package is only read" postcondition true.
///
/// # Errors
/// [`AuditError::WorkDir`] when the file cannot be written — propagated for the
/// reason `crate::index_report::write` propagates its own: the roll-up is a
/// member of the deliverable, not a cosmetic extra.
///
/// Test: `debt_rollup_tests::the_json_lands_beside_the_index`.
pub fn write(rollup: &DebtRollup, generated_at: &str, dir: &Path) -> Result<(), AuditError> {
    let path = dir.join(REPORT_FILE);
    std::fs::write(&path, to_json(rollup, generated_at))
        .map_err(|source| AuditError::WorkDir { path, source })
}

#[cfg(test)]
mod debt_rollup_tests {
    use super::*;

    /// A manifest carrying the rows the collectors write, plus keys this reader
    /// does not model — the shape a CWE-tagging or OSV collector produces.
    fn manifest(dir: &Path, name: &str, rows: &[(&str, &str)]) -> PathBuf {
        let path = dir.join(name);
        let mut text = String::from("[report]\ntitle = \"Acme\"\nfindings = [\n");
        for (severity, category) in rows {
            text.push_str(&format!(
                "  {{ category = \"{category}\", id = \"X\", package = \"p\", version = \"1\", \
                 severity = \"{severity}\", title = \"t\", cwe = \"CWE-1\" }},\n"
            ));
        }
        text.push_str("]\n");
        std::fs::write(&path, text).expect("write manifest");
        path
    }

    /// The fixture the arithmetic tests count: three repositories, four
    /// dimensions, both bands the collectors emit.
    fn three_repositories() -> (tempfile::TempDir, DebtRollup) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let api = manifest(
            tmp.path(),
            "api.toml",
            &[
                ("RED", "dependencies"),
                ("AMBER", "dependencies"),
                ("RED", "secrets"),
            ],
        );
        let web = manifest(
            tmp.path(),
            "web.toml",
            &[("AMBER", "license"), ("AMBER", "churn")],
        );
        let ops = manifest(tmp.path(), "ops.toml", &[("RED", "secrets")]);
        let rollup = from_manifests([
            ("acme-api".to_owned(), api),
            ("acme-web".to_owned(), web),
            ("acme-ops".to_owned(), ops),
        ]);
        (tmp, rollup)
    }

    #[test]
    fn a_manifest_yields_its_findings() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = manifest(
            tmp.path(),
            "manifest.toml",
            &[("RED", "dependencies"), ("AMBER", "license")],
        );
        assert_eq!(
            read_findings(&path),
            vec![
                ("RED".to_owned(), "dependencies".to_owned()),
                ("AMBER".to_owned(), "license".to_owned()),
            ]
        );
    }

    #[test]
    fn an_unreadable_manifest_contributes_nothing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let missing = tmp.path().join("nope.toml");
        assert!(read_findings(&missing).is_empty(), "absent is fine");

        let malformed = tmp.path().join("bad.toml");
        std::fs::write(&malformed, "this is not toml = = =").expect("write");
        assert!(read_findings(&malformed).is_empty(), "malformed is fine");

        let rollup = from_manifests([("acme".to_owned(), missing)]);
        assert!(rollup.is_empty(), "a failed unit does not sink the roll-up");
    }

    #[test]
    fn three_repositories_roll_up_into_one_block() {
        let (_tmp, rollup) = three_repositories();
        assert_eq!(rollup.total, 6);
        assert_eq!(rollup.by_tier["RED"], 3);
        assert_eq!(rollup.by_tier["AMBER"], 3);
        assert_eq!(rollup.by_dimension["dependencies"], 2);
        assert_eq!(rollup.by_dimension["secrets"], 2);
        assert_eq!(rollup.by_dimension["license"], 1);
        assert_eq!(rollup.by_dimension["churn"], 1);
        assert_eq!(rollup.by_repo["acme-api"]["RED"], 2);
        assert_eq!(rollup.by_repo["acme-web"]["AMBER"], 2);
        assert_eq!(rollup.by_repo["acme-ops"]["RED"], 1);
        assert_eq!(rollup.by_tier_dimension["RED"]["secrets"], 2);
        assert_eq!(rollup.by_tier_dimension["AMBER"]["churn"], 1);
        assert!(
            !rollup.by_tier_dimension["RED"].contains_key("license"),
            "a cell nothing landed in is absent, not zero"
        );
    }

    /// The invariant the whole block exists to make checkable: no consumer can
    /// pick a marginal that disagrees with the total.
    #[test]
    fn every_marginal_sums_to_the_total() {
        let (_tmp, rollup) = three_repositories();
        let sum = |m: &BTreeMap<String, usize>| m.values().sum::<usize>();
        assert_eq!(sum(&rollup.by_tier), rollup.total, "by_tier");
        assert_eq!(sum(&rollup.by_dimension), rollup.total, "by_dimension");
        assert_eq!(
            rollup.by_repo.values().map(sum).sum::<usize>(),
            rollup.total,
            "by_repo"
        );
        assert_eq!(
            rollup.by_tier_dimension.values().map(sum).sum::<usize>(),
            rollup.total,
            "by_tier_dimension"
        );
        // The two two-dimensional tables agree on their shared axis as well.
        for (tier, count) in &rollup.by_tier {
            assert_eq!(sum(&rollup.by_tier_dimension[tier]), *count, "{tier}");
        }
    }

    #[test]
    fn an_unbanded_finding_is_counted_not_dropped() {
        let mut rollup = DebtRollup::default();
        rollup.tally("acme", "  ", "");
        rollup.tally("acme", " red ", " secrets ");
        assert_eq!(rollup.total, 2);
        assert_eq!(rollup.by_tier[UNSPECIFIED_TIER], 1);
        assert_eq!(rollup.by_dimension[UNCATEGORISED_DIMENSION], 1);
        assert_eq!(rollup.by_tier["RED"], 1, "a band is folded to upper case");
        assert_eq!(rollup.by_dimension["secrets"], 1, "a dimension is trimmed");
    }

    #[test]
    fn the_table_orders_red_before_amber() {
        let mut rollup = DebtRollup::default();
        rollup.tally("acme", "AMBER", "license");
        rollup.tally("acme", "ZEBRA", "license");
        rollup.tally("acme", "RED", "secrets");
        assert_eq!(rollup.tiers(), vec!["RED", "AMBER", "ZEBRA"]);
        let table = rollup.table();
        let red = table.find("RED").expect("RED column");
        let amber = table.find("AMBER").expect("AMBER column");
        assert!(red < amber, "{table}");
    }

    /// The renderer reads the roll-up rather than re-counting: mutate the block
    /// and the rendered totals follow it.
    #[test]
    fn the_table_reads_its_totals_from_the_rollup() {
        let (_tmp, mut rollup) = three_repositories();
        let before = rollup.table();
        assert!(
            before.contains("| **all repositories** | **3** | **3** | **6** |"),
            "{before}"
        );
        assert!(before.contains("| acme-api | 2 | 1 | 3 |"), "{before}");

        rollup.tally("acme-api", "RED", "dependencies");
        let after = rollup.table();
        assert!(
            after.contains("| **all repositories** | **4** | **3** | **7** |"),
            "{after}"
        );
        assert!(after.contains("| acme-api | 3 | 1 | 4 |"), "{after}");
    }

    #[test]
    fn an_empty_rollup_renders_nothing() {
        assert_eq!(DebtRollup::default().table(), "");
    }

    /// The closure condition's shape: a `debt_rollup` block carrying all five
    /// fields, so a consumer can read a total without touching a finding.
    #[test]
    fn the_json_carries_the_rollup_block() {
        let (_tmp, rollup) = three_repositories();
        let text = to_json(&rollup, "2026-09-04 11:00:00 -04:00");
        let parsed: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
        assert_eq!(parsed["generated_at"], "2026-09-04 11:00:00 -04:00");
        let block = &parsed["debt_rollup"];
        for field in [
            "by_tier",
            "by_dimension",
            "by_repo",
            "by_tier_dimension",
            "total",
        ] {
            assert!(!block[field].is_null(), "{field} must be present: {text}");
        }
        assert_eq!(block["total"], 6);
        assert_eq!(block["by_tier"]["RED"], 3);
        assert_eq!(block["by_dimension"]["churn"], 1);
        assert_eq!(block["by_repo"]["acme-ops"]["RED"], 1);
        assert_eq!(block["by_tier_dimension"]["RED"]["secrets"], 2);
    }

    #[test]
    fn the_json_lands_beside_the_index() {
        let (tmp, rollup) = three_repositories();
        let out = tmp.path().join("out");
        std::fs::create_dir(&out).expect("mkdir");
        write(&rollup, "2026-09-04 11:00:00 -04:00", &out).expect("written");
        let text = std::fs::read_to_string(out.join(REPORT_FILE)).expect("report.json");
        assert!(text.contains("\"debt_rollup\""), "{text}");
        assert!(text.contains("\"total\": 6"), "{text}");
    }
}
