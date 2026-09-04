//! What every repository's OSV scan adds up to, for the run index (#6780).
//!
//! Why: [`super::osv`] answers for ONE repository and writes one `osv.json`.
//! The question a recipient opens `index.md` with is about the whole run — did
//! this engagement find anything, and how bad. Answering it means reading every
//! repository's file back, which is a different job from performing a scan and
//! is why it is a different module rather than more of that one.
//!
//! What: [`rollup`], which folds the scans on disk into a [`Rollup`], and
//! [`index_section`], which renders it as the count table and worst-first list
//! `crate::index_report` appends.
//!
//! Reading the files back rather than accumulating in memory is deliberate: the
//! index then states what the bundle CARRIES, so a scan whose write failed
//! cannot be summarised as if it had landed.
//!
//! Test: `super::osv::osv_tests`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use super::osv::{ADVISORY_URL, SCAN_FILE, Scan, Severity};

/// One row of the run index's "top items" table.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct TopItem {
    /// The repository this advisory was matched in.
    pub repo: String,
    /// The advisory id.
    pub id: String,
    /// The affected package.
    pub package: String,
    /// The pinned version.
    pub version: String,
    /// OSV's own label.
    pub severity: Severity,
    /// The one-line title.
    pub title: String,
}

/// What every repository's scan adds up to, for the run index.
///
/// Why: `osv.json` is per repository, and the question a reader opens the index
/// with is "did this engagement find anything". A count table answers it before
/// any file is opened.
/// What: how many repositories carried a scan, the query and match totals, the
/// error count, one count per [`Severity`](super::osv::Severity), and the worst [`super::osv::TOP_ITEMS`] rows.
/// Test: `osv_tests::the_rollup_counts_every_repository`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Rollup {
    /// Repositories whose directory carried an `osv.json`.
    pub repos: usize,
    /// Coordinates answered across every repository.
    pub queried: usize,
    /// Packages carrying at least one advisory.
    pub matched: usize,
    /// Recorded degradations across every repository.
    pub errors: usize,
    /// Advisory count per severity label.
    pub counts: BTreeMap<Severity, usize>,
    /// The worst advisories, worst first, capped at [`super::osv::TOP_ITEMS`].
    pub top: Vec<TopItem>,
}

impl Rollup {
    /// True when no repository in this run carried an OSV scan.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.repos == 0
    }

    /// Every advisory counted, across all severities.
    #[must_use]
    pub fn advisories(&self) -> usize {
        self.counts.values().sum()
    }
}

/// Roll up every `osv.json` under `dirs`, in the run's own order.
///
/// A directory with no `osv.json`, or one that cannot be read, contributes
/// nothing and is not an error here: a repository the collector skipped, or one
/// whose scan failed, already stated itself in the gap list this run recorded.
/// Test: `osv_tests::the_rollup_counts_every_repository`.
#[must_use]
pub fn rollup(dirs: &[PathBuf]) -> Rollup {
    let mut rollup = Rollup::default();
    for dir in dirs {
        let name = dir
            .file_name()
            .map_or_else(|| dir.display().to_string(), |n| n.to_string_lossy().into());
        let Ok(text) = std::fs::read_to_string(dir.join(SCAN_FILE)) else {
            continue;
        };
        let Ok(scan) = serde_json::from_str::<Scan>(&text) else {
            continue;
        };
        rollup.repos += 1;
        rollup.queried += scan.queried;
        rollup.matched += scan.matched;
        rollup.errors += scan.errors.len();
        for package in &scan.packages {
            for vuln in &package.vulns {
                *rollup.counts.entry(vuln.severity).or_default() += 1;
                rollup.top.push(TopItem {
                    repo: name.clone(),
                    id: vuln.id.clone(),
                    package: package.package.clone(),
                    version: package.version.clone(),
                    severity: vuln.severity,
                    title: vuln.title(),
                });
            }
        }
    }
    rollup
        .top
        .sort_by(|a, b| a.severity.cmp(&b.severity).then_with(|| a.id.cmp(&b.id)));
    rollup.top.truncate(super::osv::TOP_ITEMS);
    rollup
}

/// The heading the run index renders this leg under.
pub const INDEX_HEADING: &str = "Known vulnerabilities (OSV)";

/// The run index's OSV section, or `""` when the producer knows nothing of it.
///
/// Why: a run with the collector switched off used to be indistinguishable from
/// one where it ran and matched nothing, and both from one where every batch
/// failed. The section states which, in the file a recipient opens first.
/// What: `None` renders nothing at all — the re-render and the return-package
/// indexes have no scan data and must not claim the leg did not run. An empty
/// roll-up renders the opt-in line. A populated one renders the count table and
/// the worst [`super::osv::TOP_ITEMS`] rows.
/// Test: `osv_tests::{a_disabled_collector_says_so_in_the_index,
/// the_index_section_counts_by_severity}`.
#[must_use]
pub fn index_section(rollup: Option<&Rollup>) -> String {
    let Some(rollup) = rollup else {
        return String::new();
    };
    let mut out = format!("## {INDEX_HEADING}\n\n");
    if rollup.is_empty() {
        out.push_str(
            "_Not run (opt-in)._ This run queried no advisory database over its dependency \
             inventory. Set `osv = true` under `[collectors]` in `engagement.toml` to turn the \
             lookup on; until then this engagement states no known-vulnerability exposure, which \
             is unassessed rather than clean.\n\n",
        );
        return out;
    }
    out.push_str(&format!(
        "OSV.dev answered for {} pinned package(s) across {} repositor(y/ies); {} carried at least \
         one advisory, and {} degradation(s) are named under each report's gaps.\n\n",
        rollup.queried, rollup.repos, rollup.matched, rollup.errors
    ));
    out.push_str("| Severity | Advisories |\n|---|---|\n");
    for severity in Severity::ALL {
        out.push_str(&format!(
            "| {} | {} |\n",
            severity.as_str(),
            rollup.counts.get(&severity).copied().unwrap_or_default()
        ));
    }
    out.push_str(&format!("| **Total** | **{}** |\n", rollup.advisories()));
    if !rollup.top.is_empty() {
        out.push_str("\nWorst first:\n\n");
        out.push_str("| Advisory | Repository | Package | Version | Severity | Summary |\n");
        out.push_str("|---|---|---|---|---|---|\n");
        for item in &rollup.top {
            out.push_str(&format!(
                "| [{}]({ADVISORY_URL}{}) | {} | {} | {} | {} | {} |\n",
                item.id,
                item.id,
                item.repo,
                item.package,
                item.version,
                item.severity.as_str(),
                item.title.replace('|', "\\|")
            ));
        }
    }
    out.push('\n');
    out
}
