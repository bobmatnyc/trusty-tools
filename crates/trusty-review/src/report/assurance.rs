//! The Assurance Scans section: deterministic scanner output (#6075, epic #6074).
//!
//! Why: the report named CVE exposure, license risk and secret leakage as
//! assurance gaps it does not fill, and that was true only because nothing
//! reached this crate with the answer. `trusty-audit` now runs the scanners and
//! declares their rows in the manifest (owner ruling 2026-08-19: the collectors
//! live there, the manifest is the interface). This module is the consumer —
//! without it the rows land in a file nothing renders, which is the same
//! silence one file further along.
//!
//! What: [`report_section`], a pure `model → string` transform appended after
//! polish, exactly as `super::investigate::report_sections` is. Appending
//! rather than filling a template placeholder is what keeps the rows out of the
//! omit-empty pass, which would drop a table it does not recognise, and what
//! keeps the section absent from every report whose manifest declares nothing.
//!
//! ## Why the rows are rendered rather than summarised
//!
//! They are a scanner's own output, not synthesis. The renderer reorders them
//! by band and groups them by collector, and changes nothing else — an id, a
//! version and a title the tool stated reach the page as the tool stated them.
//!
//! Test: `assurance_tests.rs`.

use crate::report::manifest::ManifestFinding;
use crate::report::model::ReportModel;
use crate::report::provenance::MEASURED_TAG;

/// The heading this section renders under, and the one the disclaimers point at.
pub const HEADING: &str = "Assurance Scans";

/// Render the Assurance Scans section, or `""` when nothing was scanned.
///
/// Why: a report whose manifest declares no findings must be byte-identical to
/// one produced before this section existed — a heading over an empty table
/// would read as a scan that found nothing, which is the false clean claim this
/// whole epic exists to remove. A producer that scanned and found nothing says
/// so in Gaps & Caveats instead, so the empty case here genuinely means "no
/// collector ran".
/// What: one `###` subsection per collector category, in the order the
/// categories first appear, each a table sorted RED band first and then by the
/// order the collector reported. Rows are rendered verbatim; nothing is
/// dropped, capped, or re-worded.
/// Test: `assurance_tests::{a_declared_finding_reaches_the_report,
/// no_findings_render_nothing_at_all}`.
#[must_use]
pub fn report_section(model: &ReportModel) -> String {
    if model.findings.is_empty() {
        return String::new();
    }
    let mut out = format!("\n\n## {HEADING}\n\n");
    out.push_str(&format!(
        "_Deterministic scanner output collected against the target repositories{MEASURED_TAG}, \
         recorded verbatim. These rows are tool findings, not the LLM's readings that populate \
         Security Posture. A scan that did not run is named under Gaps & Caveats rather than \
         omitted here._\n",
    ));
    for category in categories(&model.findings) {
        out.push_str(&format!("\n### {}\n\n", subsection_title(category)));
        out.push_str(&table(&model.findings, category));
    }
    out
}

/// The collector categories present, in the order they first appear.
///
/// First-appearance order rather than alphabetical: the producer writes its
/// collectors in the order it ran them, and a reader following the report is
/// better served by that than by an ordering neither side chose.
fn categories(findings: &[ManifestFinding]) -> Vec<&str> {
    let mut seen: Vec<&str> = Vec::new();
    for finding in findings {
        let category = finding.category.trim();
        if !seen.contains(&category) {
            seen.push(category);
        }
    }
    seen
}

/// The heading one collector's subsection renders under.
///
/// The three categories epic #6074 defines get a name a due-diligence reader
/// recognises; anything else is titled by its own category string, so a
/// collector added after this crate was built still renders rather than
/// arriving under a wrong label.
fn subsection_title(category: &str) -> String {
    match category {
        "dependencies" => "Dependency CVE Exposure".to_string(),
        "license" => "License / IP Exposure".to_string(),
        "secrets" => "Secret Leakage".to_string(),
        // #6079: git-churn hotspots. Not an assurance SCAN in the sense the
        // other three are — nothing here is a vulnerability — but the same
        // shape: a deterministic measurement of the target repository the
        // report states verbatim rather than infers.
        "churn" => "Change Hotspots".to_string(),
        "" => "Uncategorised Findings".to_string(),
        other => other.to_string(),
    }
}

/// One collector's findings as a markdown table, worst band first.
fn table(findings: &[ManifestFinding], category: &str) -> String {
    let mut rows: Vec<&ManifestFinding> = findings
        .iter()
        .filter(|f| f.category.trim() == category)
        .collect();
    // A stable sort, so within a band the collector's own order survives.
    rows.sort_by_key(|f| band_rank(&f.severity));

    let mut out = String::from("| Finding | Component | Version | Severity | Summary |\n");
    out.push_str("|---|---|---|---|---|\n");
    for row in rows {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} |\n",
            linked(row),
            cell(&row.package),
            cell(&row.version),
            cell(&row.severity),
            cell(&row.title),
        ));
    }
    out
}

/// Sort key: RED before AMBER before every band this crate does not recognise.
///
/// An unrecognised band sorts last rather than erroring — the producer owns the
/// vocabulary, and a band added later must still render.
fn band_rank(severity: &str) -> u8 {
    match severity.trim().to_ascii_uppercase().as_str() {
        "RED" => 0,
        "AMBER" => 1,
        _ => 2,
    }
}

/// The finding cell: its id, linked when the collector supplied a URL.
fn linked(finding: &ManifestFinding) -> String {
    let id = cell(&finding.id);
    match finding
        .url
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty())
    {
        Some(url) => format!("[{id}]({url})"),
        None => id,
    }
}

/// One cell, with the pipes that would break the table escaped.
///
/// An advisory title is upstream prose and may contain anything; an unescaped
/// `|` silently splits the row into the wrong columns.
fn cell(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return "—".to_string();
    }
    trimmed.replace('|', "\\|").replace('\n', " ")
}

#[cfg(test)]
#[path = "assurance_tests.rs"]
mod assurance_tests;
