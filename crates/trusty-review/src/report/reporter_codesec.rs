//! Code Quality & Architecture and Security Posture: deterministic
//! re-projections of already-loaded `AnalyzeMetrics` data (#6004).
//!
//! Why: both new sections restate data the model already carries — nothing
//! here is a new data source. Code Quality re-projects the complexity
//! distribution, LoC/tech-stack, and maintainability (refactor/code-smell)
//! findings already used by §3/§4/§5; Security Posture re-projects the SAME
//! RED/AMBER findings §5 lists, grouped by tool/domain — the promoted, now
//! actually-filled, successor to the old §6.1 `security_violations_table`
//! block (previously template scaffolding with no reporter fill code at
//! all). Splitting this out of `reporter.rs` keeps that file under the
//! 500-SLOC production cap.
//! What: [`push_code_quality_rows`] and [`push_security_violation_rows`],
//! both called from `reporter::build_scope`.
//! Test: `reporter_codesec_tests.rs`.

use super::fill::Scope;
use super::investigate::SECURITY_DIMENSION;
use super::metrics::{AnalyzeMetrics, Severity};
use super::model::{ReportModel, RepositoryReport};
use super::provenance::{Provenance, tag};
use super::reporter_facts::{repo_languages, repo_loc};

/// `MetricFinding.category` value `analyze_adapter::refactor_finding` always
/// writes — refactor/code-smell findings are maintainability by construction,
/// so this is the seam between "code quality" and "security posture" below.
const MAINTAINABILITY_CATEGORY: &str = "maintainability";

/// How many languages the Code Quality row names.
const CQ_LANGUAGE_COUNT: usize = 3;

/// The Security Posture text when the security dimension recorded no GREEN
/// finding — an explicit statement, never a blank cell.
const NO_CLEAN_SIGNALS: &str = "No clean security signals were recorded: the investigation raised no GREEN finding in \
     this dimension that cites the file it was read from. That is an absence of positive \
     evidence, not a negative result.";

/// The Code Quality maintainability cell when the trusty-analyze lane
/// contributed nothing.
///
/// #6080: maintainability findings come from trusty-analyze alone, but the
/// count was taken from `metrics.findings`, which the investigation pass also
/// writes into. A run whose analyze data was rejected as stale therefore
/// printed `0 ⁽ᵐ⁾` — a measured zero asserting a measurement nobody made, on
/// the same page whose Gaps & Caveats said the lane was rejected.
const MAINTAINABILITY_NOT_ASSESSED: &str =
    "Not assessed — the trusty-analyze lane contributed no data (see Gaps & Caveats)";

/// Push one `code_quality_row` per repository carrying metrics.
///
/// Why: re-projects complexity distribution, tech-stack/LoC, and the
/// maintainability-finding count into a dedicated section (#6004) rather than
/// requiring a reader to cross-reference §3/§4/§5.
/// What: `cq_loc`/`cq_tech` take the SAME source precedence Key Facts and the
/// §4 Profile table use ([`repo_loc`]/[`repo_languages`] — declared metrics
/// when they carry a positive figure, else the built-in `RepoScan`), so the two
/// sections cannot disagree about one repository's size. #6137: reading
/// `metrics.loc.total`/`metrics.primary_languages` alone printed "not stated in
/// source data" for both cells of a run whose `--analyze` fetch left `loc`
/// empty by design (the adapter leaves it to the scanner) while §4.1 rendered
/// 1,553,771 lines from the scan two sections earlier. `cq_complexity` and
/// `cq_maintainability_count` have no scan equivalent and stay metrics-only; a
/// real zero maintainability count is a stated fact (tagged, not omitted),
/// never a gap. A repository carrying no data at all in any of the four cells
/// is skipped (its row would be 100% honesty markers, which the omit-empty pass
/// would drop anyway).
/// Test: `reporter_codesec_tests::{code_quality_rows_reproject_metrics,
/// code_quality_loc_and_tech_fall_back_to_the_scan}`.
pub fn push_code_quality_rows(root: &mut Scope, model: &ReportModel) {
    for repo in &model.repositories {
        let loc = repo_loc(repo);
        let langs = top_languages(repo);
        let complexity = repo.metrics.as_ref().and_then(bucket_summary);
        // #6080: only a repository whose analyze lane actually landed can state
        // a maintainability count; otherwise the cell names the gap.
        let maintainability = match repo.analyze_gap.is_some() {
            true => None,
            false => repo.metrics.as_ref().map(maintainability_count),
        };
        if loc == 0 && langs.is_empty() && complexity.is_none() && maintainability.is_none() {
            continue;
        }
        let mut row = Scope::new();
        row.set("cq_app_name", repo.name.clone());
        if loc > 0 {
            row.set("cq_loc", tag(loc.to_string(), Provenance::Measured));
        }
        if !langs.is_empty() {
            row.set("cq_tech", tag(langs.join(", "), Provenance::Measured));
        }
        if let Some(summary) = complexity {
            row.set("cq_complexity", tag(summary, Provenance::Measured));
        }
        match maintainability {
            Some(count) => row.set(
                "cq_maintainability_count",
                tag(count.to_string(), Provenance::Measured),
            ),
            None => row.set(
                "cq_maintainability_count",
                tag(MAINTAINABILITY_NOT_ASSESSED, Provenance::NotStated),
            ),
        }
        root.push_block("code_quality_row", row);
    }
}

/// The top [`CQ_LANGUAGE_COUNT`] languages by LoC for one repository, from
/// whichever source [`repo_languages`] selected.
fn top_languages(repo: &RepositoryReport) -> Vec<String> {
    let mut langs: Vec<&super::metrics::LanguageLoc> = repo_languages(repo).iter().collect();
    langs.sort_by_key(|l| std::cmp::Reverse(l.loc));
    langs
        .into_iter()
        .take(CQ_LANGUAGE_COUNT)
        .map(|l| l.language.clone())
        .collect()
}

/// Count one repository's non-GREEN maintainability findings.
fn maintainability_count(m: &AnalyzeMetrics) -> usize {
    m.findings
        .iter()
        .filter(|f| f.severity != Severity::Green && f.category == MAINTAINABILITY_CATEGORY)
        .count()
}

/// Push one `security_violations_table` row per application carrying a RED or
/// AMBER finding in the SECURITY dimension, and state that dimension's clean
/// signals.
///
/// Why (#6004): promotes §6.1 out of Risk Registers into its own top-level
/// Security Posture section. #6137 narrows what it counts: the table used to
/// group EVERY non-maintainability finding by category, so a run whose
/// investigation covers six due-diligence dimensions printed error-handling,
/// state-management, scalability, test-coverage and dependency findings under a
/// "Security Posture" heading labelled "Total violations" — 90 of one report's
/// 110 rows were not security findings at all. Only
/// [`SECURITY_DIMENSION`] findings are security findings. The GREEN findings in
/// that same dimension are the clean signals a reader weighs the count against,
/// and dropping them (the no-green-analysis rule bans ELABORATION, not
/// acknowledgement) left the section reading as unmitigated risk.
/// What: counts non-GREEN [`SECURITY_DIMENSION`] findings per repository,
/// pushing one row (`app_name`/`violation_domain`/`violation_count`, the block's
/// unchanged column shape) per repository with at least one; sets
/// `security_clean_signals` from that dimension's GREEN findings that carry a
/// `file:line`, each rendered with it, or [`NO_CLEAN_SIGNALS`] when none does.
/// Test: `reporter_codesec_tests::{security_rows_count_only_the_security_dimension,
/// security_section_credits_clean_signals,
/// security_section_states_when_no_clean_signal_exists,
/// an_uncited_green_is_not_a_clean_signal}`.
pub fn push_security_violation_rows(root: &mut Scope, model: &ReportModel) {
    let mut clean: Vec<String> = Vec::new();
    let mut assessed = false;
    for repo in &model.repositories {
        let Some(m) = &repo.metrics else { continue };
        assessed = true;
        let mut count = 0u64;
        for f in m
            .findings
            .iter()
            .filter(|f| f.category == SECURITY_DIMENSION)
        {
            if f.severity == Severity::Green {
                // #6080: a clean signal must name the file it was read from.
                // An uncited GREEN title is an assertion, and one of them —
                // "Raw SQL string interpolation via multi-line concatenation
                // for PR upsert" — was a defect listed as a strength with
                // nothing in the report a reader could check it against.
                if !f.component.trim().is_empty() {
                    clean.push(format!("{} (`{}`)", f.title, f.component.trim()));
                }
            } else {
                count += 1;
            }
        }
        if count == 0 {
            continue;
        }
        let mut row = Scope::new();
        row.set("app_name", repo.name.clone());
        row.set(
            "violation_domain",
            tag(SECURITY_DIMENSION, Provenance::Measured),
        );
        row.set(
            "violation_count",
            tag(count.to_string(), Provenance::Measured),
        );
        root.push_block("security_violations_table", row);
    }
    // A run that assessed nothing states nothing: the scalar is left unset, so
    // the honesty rule collapses the line and Gaps & Caveats names the section.
    // Claiming "no clean signal was recorded" for a run with no findings at all
    // would be the same false-clean claim this section exists to avoid.
    if !assessed {
        return;
    }
    let signals = if clean.is_empty() {
        tag(NO_CLEAN_SIGNALS, Provenance::NotStated)
    } else {
        tag(
            format!(
                "Clean signals found in this dimension: {}.",
                clean.join("; ")
            ),
            Provenance::Measured,
        )
    };
    root.set("security_clean_signals", signals);
}

/// A compact "label: count (pct%)" complexity summary for one repository's
/// `AnalyzeMetrics.complexity` — the per-app counterpart of
/// `reporter_facts::complexity_profile`'s engagement-wide merge.
///
/// #6691: `synthesize_prompt::repo_profile_lines` states this same string to the
/// synthesis model. Reading it from here rather than re-deriving it is what
/// makes the prose and the `cq_complexity` cell beside it the same measurement,
/// so a paragraph denying the distribution the table renders cannot be written
/// from data that omitted it.
/// Test: `reporter_codesec_tests::code_quality_rows_reproject_metrics`,
/// `synthesize_tests::digest_carries_the_complexity_distribution_the_table_renders`.
pub(super) fn bucket_summary(m: &AnalyzeMetrics) -> Option<String> {
    let total: u64 = m.complexity.buckets.iter().map(|b| b.count).sum();
    if total == 0 {
        return None;
    }
    let parts: Vec<String> = m
        .complexity
        .buckets
        .iter()
        .map(|b| {
            let pct = (b.count as f64 / total as f64) * 100.0;
            format!("{}: {} ({pct:.0}%)", b.label, b.count)
        })
        .collect();
    Some(parts.join("; "))
}

#[cfg(test)]
#[path = "reporter_codesec_tests.rs"]
mod tests;
