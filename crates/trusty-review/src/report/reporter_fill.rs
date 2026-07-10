//! Reporter fill helpers: self-derived metadata + scan/metrics profile (wave 2).
//!
//! Why: the reporter's scope-building grew with wave 2 (self-derived vendor
//! metadata, the analyst-instructions section, the self-described scoring model,
//! and the measured-vs-declared profile merge); factoring these cohesive helpers
//! out keeps `reporter.rs` under the SLOC cap and gives the fill logic one home.
//! What: [`crate_version`], [`instructions_block`], [`set_scoring_model`],
//! [`fill_profile`], and [`format_frameworks`] — all `pub(super)` so `reporter.rs`
//! calls them while they stay crate-internal.
//! Test: exercised via the reporter render tests (`reporter_tests.rs`).

use super::fill::Scope;
use super::model::ReportModel;
use super::model::RepositoryReport;
use super::provenance::{Provenance, tag};
use super::scan::RepoScan;

/// The crate version string, for self-derived metadata fields.
///
/// Why: the vendor/methodology and report-version fields embed the generator's
/// own version so a reader can tell which build produced the report.
/// What: returns `CARGO_PKG_VERSION` resolved at compile time.
/// Test: exercised by `reporter_tests.rs::render_contains_expected`.
pub(super) fn crate_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Build the verbatim "Analyst Instructions" section, or an empty string.
///
/// Why: #2340 requires every report to record the analyst brief verbatim (a
/// declared-provenance section) so the reader knows what the analysis was asked
/// to focus on; when no brief was supplied the placeholder renders empty rather
/// than as a honesty marker.
/// What: returns a markdown section (heading + source note + the verbatim brief)
/// when `model.instructions` is set, else `""`.
/// Test: `reporter_tests.rs::reporter_records_instructions_verbatim`.
pub(super) fn instructions_block(model: &ReportModel) -> String {
    let Some(text) = &model.instructions else {
        return String::new();
    };
    let source = model
        .instructions_source
        .as_deref()
        .unwrap_or("analyst instructions");
    format!(
        "## Analyst Instructions\n\n_Focus directives supplied by the analyst ({source}) (provenance: declared)._\n\n{text}\n"
    )
}

/// Fill Section 3 with trusty-review's own normalized scoring model (#2342.2).
///
/// Why: the scoring model is defined BY the tool — rendering it as "not stated"
/// was the exact defect owner feedback flagged; these are measured self-facts.
/// What: sets the generic template's Section 3 scalars to trusty-review's 0–100
/// RED/AMBER/GREEN model.  The CAST template hard-codes its own CAST scale and
/// ignores these scalars.
/// Test: `reporter_tests.rs::reporter_self_describes_scoring_model`.
pub(super) fn set_scoring_model(root: &mut Scope) {
    let m = Provenance::Measured;
    root.set(
        "native_scale_description",
        tag(
            "trusty-review normalized 0–100 quality scale (higher is healthier)",
            m,
        ),
    );
    root.set(
        "native_band_definitions",
        tag(
            "RED < 33 (critical); AMBER 33–66 (medium risk); GREEN > 66 (healthy)",
            m,
        ),
    );
    root.set(
        "normalized_mapping_formula",
        tag("already 0–100; RED/AMBER/GREEN bands applied directly", m),
    );
    root.set("red_threshold", tag("< 33", m));
    root.set("amber_threshold", tag("33–66", m));
    root.set("green_threshold", tag("> 66", m));
    root.set(
        "aggregation_method",
        tag(
            "severity bands from repository inspection; application posture aggregates its findings",
            m,
        ),
    );
    root.set(
        "benchmark_population",
        tag(
            "cross-repo corpus placement when --benchmark is enabled; otherwise not computed",
            m,
        ),
    );
}

/// Fill a repository's profile fields, preferring declared external metrics over
/// measured repo-scan values, with a provenance marker on each (#2342.3/.4).
///
/// Why: an external trusty-analyze metrics JSON is ENRICHMENT — where it and the
/// built-in scan both provide a figure the declared metrics win (documented
/// precedence); where only the scan has it, the measured figure still makes the
/// report substantive.  Every filled value carries its provenance so a reader can
/// audit it; a field neither source provides is left unset (→ omitted → Gaps).
/// What: sets `app_tech_stack`, `app_loc`, `app_file_counts`, and `app_frameworks`
/// from metrics (declared) when present, else from the scan (measured).
/// Test: `reporter_tests.rs::{reporter_scans_repo_baseline,
/// reporter_declared_metrics_win_over_scanned}`.
pub(super) fn fill_profile(scope: &mut Scope, repo: &RepositoryReport) {
    let metrics = repo.metrics.as_ref();
    let scan = repo.scan.as_ref();

    // Tech stack: declared languages win over measured scan languages.
    let stack = metrics
        .map(|m| m.primary_languages(4))
        .filter(|l| !l.is_empty())
        .map(|l| (l, Provenance::Declared))
        .or_else(|| {
            scan.map(|s| s.primary_languages(4))
                .filter(|l| !l.is_empty())
                .map(|l| (l, Provenance::Measured))
        });
    if let Some((langs, prov)) = stack {
        scope.set("app_tech_stack", tag(langs.join(", "), prov));
    }

    // Total LoC: declared metrics figure wins over the measured scan count.
    let loc = metrics
        .filter(|m| m.loc.total > 0)
        .map(|m| (m.loc.total, Provenance::Declared))
        .or_else(|| {
            scan.filter(|s| s.total_loc > 0)
                .map(|s| (s.total_loc, Provenance::Measured))
        });
    if let Some((n, prov)) = loc {
        scope.set("app_loc", tag(n.to_string(), prov));
    }

    // File / function counts: declared metrics carry both; the scan carries the
    // file count only (function counting needs a lexer the scan does not run).
    if let Some(m) = metrics.filter(|m| m.counts.files > 0 || m.counts.functions > 0) {
        scope.set(
            "app_file_counts",
            tag(
                format!("{} files, {} functions", m.counts.files, m.counts.functions),
                Provenance::Declared,
            ),
        );
    } else if let Some(s) = scan.filter(|s| s.file_count > 0) {
        scope.set(
            "app_file_counts",
            tag(format!("{} files", s.file_count), Provenance::Measured),
        );
    }

    // Frameworks / manifests: measured from the repo scan only.
    if let Some(fw) = scan.map(format_frameworks).filter(|f| !f.is_empty()) {
        scope.set("app_frameworks", tag(fw, Provenance::Measured));
    }
}

/// Render a scan's detected frameworks as a compact one-line summary.
///
/// Why: naming the build manifests and their top dependencies is the highest-value
/// measured signal for "what is this built on".
/// What: joins each framework as `manifest: name (deps: a, b, …)`, omitting the
/// name/deps parenthetical when absent; returns an empty string when none.
/// Test: `reporter_tests.rs::reporter_scans_repo_baseline`.
pub(super) fn format_frameworks(scan: &RepoScan) -> String {
    scan.frameworks
        .iter()
        .map(|f| {
            let name = if f.name.is_empty() {
                String::new()
            } else {
                format!(" {}", f.name)
            };
            let deps = if f.deps.is_empty() {
                String::new()
            } else {
                format!(" (deps: {})", f.deps.join(", "))
            };
            format!("{}:{name}{deps}", f.manifest)
        })
        .collect::<Vec<_>>()
        .join("; ")
}
