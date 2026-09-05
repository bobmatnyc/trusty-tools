//! What share of each repository the investigation pass actually read, for the
//! run index (#6784).
//!
//! Why: the coverage figure exists — `trusty-review` writes
//! `files_examined`/`total_files` per repository into the report's JSON twin and
//! states it in that report's own coverage section. What no reader saw is the
//! roll-up. A 59-repository bundle delivers 59 reports, and the question "how
//! much of this estate was actually read" was answerable only by opening every
//! one of them; the field run behind #6784 read 18-36% of its five largest
//! repositories and nothing in the bundle said so where a recipient would look.
//! The index is the file a recipient opens first, so the figure belongs there.
//!
//! What: [`rollup`], which reads each unit's report JSON back off disk and folds
//! the per-repository figures into a [`Rollup`], and [`index_section`], which
//! renders it as the table `crate::index_report` appends.
//!
//! Reading the files back rather than accumulating in memory is deliberate, for
//! the reason [`super::osv_rollup`] records: the index then states what the
//! bundle CARRIES, so a report whose write failed cannot be summarised as if it
//! had landed.
//!
//! Test: `super::coverage_rollup_tests`.

use std::path::{Path, PathBuf};

/// One repository's investigation coverage, as its report recorded it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RepoCoverage {
    /// The application name the report gave it.
    pub name: String,
    /// Files the investigation pass sent to the model.
    pub examined: usize,
    /// Tracked files in the repository — the denominator.
    pub eligible: usize,
    /// True when this repository's report says its analyze lane assessed
    /// nothing (#6811).
    ///
    /// Why: the two facts travel together. A repository read at 6% by an
    /// investigation pass that DID run is a different artifact from one whose
    /// static-analysis lane never ran at all, and a reader deciding how much
    /// weight to give a report needs both in the same row.
    pub analyze_lane_dead: bool,
}

impl RepoCoverage {
    /// Examined files as a percentage of eligible ones, rounded to one decimal.
    ///
    /// A repository with no tracked files has no share to state and reads as
    /// `0.0` — the count columns beside it already say the denominator was zero.
    #[must_use]
    pub fn share(&self) -> f64 {
        if self.eligible == 0 {
            return 0.0;
        }
        #[allow(clippy::cast_precision_loss)]
        let share = (self.examined as f64 / self.eligible as f64) * 100.0;
        (share * 10.0).round() / 10.0
    }
}

/// What every repository's investigation coverage adds up to, for the run index.
///
/// Why: see the module doc — the per-repository figure existed and the estate
/// figure did not.
/// What: one row per repository whose report carried a coverage record, plus the
/// two totals. Worst-covered first, because that is the row a due-diligence
/// reader is looking for.
/// Test: `super::coverage_rollup_tests::the_rollup_reads_every_report`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Rollup {
    /// One row per repository, worst-covered first.
    pub repos: Vec<RepoCoverage>,
}

impl Rollup {
    /// True when no report in this run carried an investigation coverage record.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.repos.is_empty()
    }

    /// Files examined across every repository.
    #[must_use]
    pub fn examined(&self) -> usize {
        self.repos.iter().map(|r| r.examined).sum()
    }

    /// Tracked files across every repository.
    #[must_use]
    pub fn eligible(&self) -> usize {
        self.repos.iter().map(|r| r.eligible).sum()
    }

    /// Repositories whose analyze lane assessed nothing (#6811).
    #[must_use]
    pub fn analyze_lanes_dead(&self) -> usize {
        self.repos.iter().filter(|r| r.analyze_lane_dead).count()
    }
}

/// The phrase `trusty-review` leads a dead analyze lane with (#6811).
///
/// Why: the same one-phrase-to-key-on contract
/// [`super::SEARCH_TIER_HEADLINE`] establishes. `trusty-review` writes it at the
/// head of the report's Gaps & Caveats and this module matches it; two spellings
/// would mean a bundle whose index disagrees with the reports it indexes.
pub const ANALYZE_LANE_DEAD_HEADLINE: &str = "trusty-analyze lane DID NOT RUN";

/// Roll up the investigation coverage of every report under `dirs`, in the run's
/// own order.
///
/// A directory with no report JSON, or one that cannot be read or parsed,
/// contributes nothing and is not an error here: a repository whose render
/// failed already states itself in the index's own failure column.
/// Test: `super::coverage_rollup_tests::the_rollup_reads_every_report`.
#[must_use]
pub fn rollup(dirs: &[PathBuf]) -> Rollup {
    let mut repos: Vec<RepoCoverage> = Vec::new();
    for dir in dirs {
        for path in report_jsons(dir) {
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            repos.extend(read_coverage(&text));
        }
    }
    // Worst-covered first; ties broken by name so two runs over the same bundle
    // render the same table.
    repos.sort_by(|a, b| {
        a.share()
            .partial_cmp(&b.share())
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.name.cmp(&b.name))
    });
    Rollup { repos }
}

/// Every `.json` directly inside `dir`, in a stable order.
///
/// Why: the report's stem is the engagement's slug, which this process does not
/// know — `trusty-review` derives it from the manifest. So the file is found by
/// extension rather than by name, exactly as
/// `tga::audit::review::require_rendered_report_carries_synthesis` finds it. A
/// `.json` that is not a report parses to no coverage record and drops out in
/// [`read_coverage`].
fn report_jsons(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    paths.sort();
    paths
}

/// The coverage rows one report JSON states, or none when it states none.
///
/// Why: a pure function over the text, so every shape is tested as a literal
/// fixture rather than reconstructed from whichever version of `ReportModel`
/// happens to be linked in — the split
/// `tga::audit::review::json_carries_synthesis` uses for the same reason. tga
/// and trusty-review meet at a file, not at a Cargo edge (DOC-67 §5), so the
/// renderer that wrote this file may be a different version from the one this
/// crate was built against.
/// What: one row per `investigation.repos[]` entry carrying a `coverage` object,
/// with `analyze_lane_dead` read off the report's own `gaps` list — a
/// report-level fact, so every row of one report carries the same value.
/// Anything that is not JSON, or is JSON without that shape, yields no rows.
/// Test: `super::coverage_rollup_tests::{the_rollup_reads_every_report,
/// a_json_that_is_not_a_report_contributes_nothing}`.
#[must_use]
pub fn read_coverage(report_json: &str) -> Vec<RepoCoverage> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(report_json) else {
        return Vec::new();
    };
    let analyze_lane_dead = value
        .get("gaps")
        .and_then(|g| g.as_array())
        .is_some_and(|gaps| {
            gaps.iter()
                .filter_map(|g| g.as_str())
                .any(|g| g.contains(ANALYZE_LANE_DEAD_HEADLINE))
        });
    let Some(repos) = value
        .get("investigation")
        .and_then(|i| i.get("repos"))
        .and_then(|r| r.as_array())
    else {
        return Vec::new();
    };
    repos
        .iter()
        .filter_map(|repo| {
            let coverage = repo.get("coverage")?;
            let usize_at = |key: &str| {
                coverage
                    .get(key)
                    .and_then(serde_json::Value::as_u64)
                    .map(|n| usize::try_from(n).unwrap_or(usize::MAX))
            };
            Some(RepoCoverage {
                name: repo
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("(unnamed)")
                    .to_owned(),
                examined: usize_at("files_examined")?,
                eligible: usize_at("total_files")?,
                analyze_lane_dead,
            })
        })
        .collect()
}

/// The heading the run index renders this leg under.
pub const INDEX_HEADING: &str = "Investigation coverage";

/// How many rows the table names before it stops.
///
/// Why: a 59-repository bundle's table is a wall, and the rows a reader needs
/// are the worst-covered ones. The totals line above it already carries the
/// whole estate.
pub const TOP_ROWS: usize = 15;

/// The run index's investigation-coverage section, or `""` when the producer
/// knows nothing of it.
///
/// Why: `None` renders nothing at all — a producer with no reports to read must
/// not claim the pass did not run, the same distinction
/// [`super::osv_rollup::index_section`] draws.
/// What: an empty roll-up renders the "not recorded" line, which is the state a
/// bundle of reports written by a renderer too old to record coverage lands in.
/// A populated one renders the estate totals and the worst [`TOP_ROWS`] rows.
/// Test: `super::coverage_rollup_tests::{the_index_section_states_the_estate_share,
/// a_bundle_with_no_coverage_records_says_so}`.
#[must_use]
pub fn index_section(rollup: Option<&Rollup>) -> String {
    let Some(rollup) = rollup else {
        return String::new();
    };
    let mut out = format!("## {INDEX_HEADING}\n\n");
    if rollup.is_empty() {
        out.push_str(
            "_Not recorded._ No report in this bundle states how much of its repository the \
             investigation pass read. Treat every finding set here as covering an unstated share \
             of its codebase (issue #6784).\n\n",
        );
        return out;
    }
    let (examined, eligible) = (rollup.examined(), rollup.eligible());
    #[allow(clippy::cast_precision_loss)]
    let share = if eligible == 0 {
        0.0
    } else {
        (examined as f64 / eligible as f64) * 100.0
    };
    out.push_str(&format!(
        "The investigation pass read {examined} of {eligible} tracked file(s) across \
         {repos} repositor(y/ies) — {share:.1}% of the estate. A finding set is evidence about \
         the files that were read and states nothing about the rest.\n\n",
        repos = rollup.repos.len(),
    ));
    let dead = rollup.analyze_lanes_dead();
    if dead > 0 {
        out.push_str(&format!(
            "{dead} of those repositor(y/ies) also ran NO static-analysis pass — their finding \
             counts, complexity figures and health factors are unassessed, not clean (issue \
             #6811).\n\n",
        ));
    }
    out.push_str("Least-covered first:\n\n");
    out.push_str("| Repository | Files read | Tracked files | Coverage | Analyze lane |\n");
    out.push_str("|---|---|---|---|---|\n");
    for repo in rollup.repos.iter().take(TOP_ROWS) {
        out.push_str(&format!(
            "| {} | {} | {} | {:.1}% | {} |\n",
            repo.name,
            repo.examined,
            repo.eligible,
            repo.share(),
            if repo.analyze_lane_dead {
                "**did not run**"
            } else {
                "ran"
            },
        ));
    }
    if rollup.repos.len() > TOP_ROWS {
        out.push_str(&format!(
            "\n_{} further repositor(y/ies) are covered at or above the rows above._\n",
            rollup.repos.len() - TOP_ROWS
        ));
    }
    out.push('\n');
    out
}
