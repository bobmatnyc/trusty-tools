//! Deterministic Executive Summary + Top Risks for §2 (#5318).
//!
//! Why: §2 is the first section a due-diligence reader opens, and it was the
//! only section of the report with no deterministic source — `reporter.rs`
//! filled it exclusively from `--synthesize` LLM prose. `tga audit` never
//! passes `--synthesize` (see `tga::audit::review`'s `invoke`), so every AUDIT
//! report collapsed §2 to `_No data available — see Gaps & Caveats._` while
//! carrying real RED/AMBER findings two sections below. The measured data the
//! report already renders answers the scope-and-posture questions §2 exists to
//! answer; rolling it up here removes §2's dependence on an LLM without
//! inventing anything.
//! What: [`compose`] returns [`ExecSummary::Composed`] — a roll-up paragraph
//! (applications, size, language mix, RED/AMBER counts, the dimensions and the
//! application they concentrate in) with the provenance of the figures it used
//! — or [`ExecSummary::Unavailable`], which names the specific inputs that were
//! missing instead of leaving the reader the generic Gaps & Caveats pointer.
//! [`top_risks`] derives the accompanying risk rows from the same findings,
//! highest severity first. Verified synthesis prose still overwrites both when
//! `--synthesize` runs and passes the guardrail (see `reporter::build_scope`).
//! Test: `exec_summary_tests.rs`.
//!
//! # Spec References
//! - [`SPEC-TGAUDIT-08~draft`](../../../../docs/specs/DOC-67-tga-audit-mode.md#SPEC-TGAUDIT-08~draft)

use std::collections::BTreeMap;

use super::metrics::{MetricFinding, Severity};
use super::model::{ReportModel, RepositoryReport};
use super::provenance::Provenance;

/// Max application names spelled out before the list is elided.
const MAX_APP_NAMES: usize = 4;
/// Max languages named in the scope sentence.
const MAX_LANGUAGES: usize = 3;
/// Max finding dimensions named in the posture sentence.
const MAX_DIMENSIONS: usize = 3;
/// Max deterministic Top Risks rows.
const MAX_RISK_ROWS: usize = 5;
/// Max characters kept from a finding's description in a risk row.
const MAX_RISK_DESCRIPTION_CHARS: usize = 160;

/// The closing sentence: what this paragraph is, so it is never mistaken for
/// an analyst's judgement.
const ROLLUP_NOTE: &str = "This summary is a deterministic roll-up of the data in the sections below, not an analyst judgement.";

/// The §2 paragraph this report can honestly render.
///
/// Why: the two outcomes are genuinely different for a reader — a roll-up is
/// report content and carries provenance; an unavailability statement is a
/// statement ABOUT the report and carries none. Keeping them distinct stops the
/// caller from tagging a "not generated" line as measured data.
/// What: `Composed` carries the paragraph and the provenance of the figures it
/// was built from; `Unavailable` carries a sentence naming the missing inputs.
/// Test: `exec_summary_tests.rs::{composes_from_metrics_findings,
/// unavailable_names_the_missing_inputs}`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExecSummary {
    /// A roll-up paragraph over the report's own measured/declared data.
    Composed {
        /// The rendered paragraph.
        text: String,
        /// Where the figures in it came from.
        provenance: Provenance,
    },
    /// Nothing to roll up; the text names which inputs were absent.
    Unavailable(String),
}

impl ExecSummary {
    /// The rendered text either way.
    pub fn text(&self) -> &str {
        match self {
            ExecSummary::Composed { text, .. } => text,
            ExecSummary::Unavailable(text) => text,
        }
    }
}

/// Compose the deterministic executive-summary paragraph for `model`.
///
/// Why: the single entry point `reporter.rs` calls before (and independently
/// of) synthesis injection, so §2 always renders something a reader can act on.
/// What: gathers per-repository size/language/finding facts, then renders the
/// scope, posture, concentration, and roll-up sentences. A manifest with no
/// repositories, or one where no repository yielded a scan, a metrics file, or
/// a live analyze fetch, returns [`ExecSummary::Unavailable`] naming exactly
/// those inputs.
/// Test: `exec_summary_tests.rs::{composes_from_metrics_findings,
/// composes_from_scan_only, unavailable_with_no_repositories,
/// unavailable_names_the_missing_inputs}`.
pub fn compose(model: &ReportModel) -> ExecSummary {
    if model.repositories.is_empty() {
        return ExecSummary::Unavailable(
            "Executive summary not generated: the manifest declared no repositories, so this \
             report has nothing to summarize."
                .to_string(),
        );
    }

    let facts = Facts::gather(&model.repositories);
    if !facts.has_data() {
        return ExecSummary::Unavailable(format!(
            "Executive summary not generated: none of the {} application(s) in this report \
             carries size or findings data — no repository supplied a `metrics` file, no \
             trusty-analyze fetch (`--analyze`) populated one, and no local checkout was \
             scannable. What that leaves unassessed is listed under Gaps & Caveats.",
            model.repositories.len()
        ));
    }

    ExecSummary::Composed {
        text: render(&facts),
        provenance: facts.provenance,
    }
}

/// One deterministic Top Risks row.
///
/// Why: the Top Risks table sits inside §2 and collapsed for the same reason
/// the paragraph did — it was synthesis-only. The report's own RED/AMBER
/// findings are exactly what the table asks for, minus a cost estimate, which
/// nothing deterministic states.
/// What: `description`/`severity`/`apps` are verbatim finding data;
/// `cost` has no deterministic source and is left to the caller (it renders as
/// the honesty marker).
/// Test: `exec_summary_tests.rs::top_risks_are_red_first_and_capped`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DeterministicRisk {
    /// The risk statement: finding title, plus its observation and component.
    pub description: String,
    /// `RED` or `AMBER`.
    pub severity: String,
    /// The owning application's display name.
    pub apps: String,
}

/// The capped Top Risks rows plus how many were eligible before the cap.
///
/// Why: the table shows at most [`MAX_RISK_ROWS`], and a reader who reads the
/// table without the paragraph above it would otherwise take five rows as the
/// whole risk picture. Carrying the pre-cap count is what lets the renderer say
/// so in the table itself. A capped top-risks table has already shipped as a
/// defect once (#2373); this time the truncation is stated.
/// What: `rows` is what renders; `eligible` counts every non-GREEN finding that
/// qualified for a row, so `eligible - rows.len()` is exactly what was dropped.
/// Test: `exec_summary_tests.rs::{top_risks_are_red_first_and_capped,
/// truncation_note_absent_when_nothing_was_dropped}`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct TopRisks {
    /// The rows to render, RED first, at most [`MAX_RISK_ROWS`].
    pub rows: Vec<DeterministicRisk>,
    /// How many findings qualified for a row before the cap was applied.
    pub eligible: usize,
}

impl TopRisks {
    /// The table's truncation caption, or `None` when nothing was dropped.
    ///
    /// Why: it must be legible to someone reading only the table, so it leads
    /// with the ratio and points at the section that carries every finding.
    /// What: `Some` only when the cap actually dropped rows.
    /// Test: `exec_summary_tests.rs::{top_risks_are_red_first_and_capped,
    /// truncation_note_absent_when_nothing_was_dropped}`.
    pub fn truncation_note(&self) -> Option<String> {
        let hidden = self.eligible.saturating_sub(self.rows.len());
        (hidden > 0).then(|| {
            format!(
                "**Top {} of {}** — {hidden} further RED/AMBER finding(s) are not listed here; \
                 section 5, Findings by Severity, lists all {} unabridged.",
                self.rows.len(),
                self.eligible,
                self.eligible,
            )
        })
    }
}

/// Derive the Top Risks rows from the report's own RED/AMBER findings.
///
/// Why: same defect as the paragraph — an empty table in the first section
/// reads as "no risks found" when the report lists findings two sections down.
/// What: every non-GREEN finding across all repositories, RED before AMBER
/// (stable within a band, so manifest and analyzer order are preserved), capped
/// at [`MAX_RISK_ROWS`] with the pre-cap count retained for
/// [`TopRisks::truncation_note`]. Empty when no repository carries findings.
/// Test: `exec_summary_tests.rs::top_risks_are_red_first_and_capped`.
pub fn top_risks(model: &ReportModel) -> TopRisks {
    let mut ranked: Vec<(u8, DeterministicRisk)> = Vec::new();
    for repo in &model.repositories {
        let Some(metrics) = &repo.metrics else {
            continue;
        };
        for f in &metrics.findings {
            let rank = match f.severity {
                Severity::Red => 0,
                Severity::Amber => 1,
                Severity::Green => continue,
            };
            ranked.push((
                rank,
                DeterministicRisk {
                    description: risk_description(f),
                    severity: if rank == 0 { "RED" } else { "AMBER" }.to_string(),
                    apps: repo.name.clone(),
                },
            ));
        }
    }
    ranked.sort_by_key(|(rank, _)| *rank);
    let eligible = ranked.len();
    TopRisks {
        rows: ranked
            .into_iter()
            .take(MAX_RISK_ROWS)
            .map(|(_, risk)| risk)
            .collect(),
        eligible,
    }
}

/// The risk statement for one finding: title, the tool's observation, and the
/// component, skipping whichever of the last two the source left empty.
fn risk_description(f: &MetricFinding) -> String {
    let mut out = f.title.trim().to_string();
    let description = truncate(f.description.trim(), MAX_RISK_DESCRIPTION_CHARS);
    if !description.is_empty() {
        out.push_str(" — ");
        out.push_str(&description);
    }
    let component = f.component.trim();
    if !component.is_empty() {
        out.push_str(&format!(" ({component})"));
    }
    out
}

/// Truncate to `max_chars` on a char boundary, appending `…` when cut.
fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    format!("{}…", s.chars().take(max_chars).collect::<String>())
}

/// The measured/declared facts the paragraph is rendered from.
///
/// Why: gathering first keeps every sentence a pure function of counted data,
/// so a test can pin the wording without building a template render.
/// What: application names, aggregate size, aggregate language mix, RED/AMBER
/// counts by dimension and by application, and the provenance of the figures.
#[derive(Debug)]
struct Facts {
    app_names: Vec<String>,
    total_loc: u64,
    total_files: u64,
    /// LoC per language, aggregated across applications.
    by_language: BTreeMap<String, u64>,
    red: usize,
    amber: usize,
    /// Non-GREEN finding counts per dimension/category.
    by_dimension: BTreeMap<String, usize>,
    /// Non-GREEN finding counts per application, RED-weighted for ranking.
    red_by_app: BTreeMap<String, usize>,
    amber_by_app: BTreeMap<String, usize>,
    /// True when at least one figure came from a declared metrics document
    /// rather than this tool's own scan.
    provenance: Provenance,
}

impl Facts {
    /// Gather the roll-up facts across every repository in the report.
    ///
    /// Why: declared metrics and the built-in scan carry the same figures with
    /// different provenance; the paragraph must state which it used, and the
    /// weaker claim wins when both contributed (mirrors
    /// `reporter_fill::fill_profile`'s declared-over-measured precedence).
    /// What: prefers a repository's metrics document over its scan for size and
    /// language mix; findings only ever come from metrics (the scan produces
    /// none).
    fn gather(repos: &[RepositoryReport]) -> Self {
        let mut f = Facts {
            app_names: Vec::new(),
            total_loc: 0,
            total_files: 0,
            by_language: BTreeMap::new(),
            red: 0,
            amber: 0,
            by_dimension: BTreeMap::new(),
            red_by_app: BTreeMap::new(),
            amber_by_app: BTreeMap::new(),
            provenance: Provenance::Measured,
        };
        for repo in repos {
            f.app_names.push(repo.name.clone());

            match (&repo.metrics, &repo.scan) {
                (Some(m), _) if m.loc.total > 0 || !m.loc.by_language.is_empty() => {
                    f.total_loc += m.loc.total;
                    f.total_files += m.counts.files;
                    for l in &m.loc.by_language {
                        *f.by_language.entry(l.language.clone()).or_default() += l.loc;
                    }
                    f.provenance = Provenance::Declared;
                }
                (_, Some(s)) => {
                    f.total_loc += s.total_loc;
                    f.total_files += s.file_count;
                    for l in &s.by_language {
                        *f.by_language.entry(l.language.clone()).or_default() += l.loc;
                    }
                }
                _ => {}
            }

            let Some(metrics) = &repo.metrics else {
                continue;
            };
            for finding in &metrics.findings {
                match finding.severity {
                    Severity::Red => {
                        f.red += 1;
                        *f.red_by_app.entry(repo.name.clone()).or_default() += 1;
                    }
                    Severity::Amber => {
                        f.amber += 1;
                        *f.amber_by_app.entry(repo.name.clone()).or_default() += 1;
                    }
                    Severity::Green => continue,
                }
                let dimension = finding.category.trim();
                if !dimension.is_empty() {
                    *f.by_dimension.entry(dimension.to_string()).or_default() += 1;
                }
                f.provenance = Provenance::Declared;
            }
        }
        f
    }

    /// True when anything measurable was gathered.
    ///
    /// Why: an unavailability statement is only honest when there genuinely was
    /// no input — a report over remote-only entries with no metrics.
    fn has_data(&self) -> bool {
        self.total_loc > 0 || self.total_files > 0 || self.red + self.amber > 0
    }

    /// How many distinct applications contributed a RED or AMBER finding.
    ///
    /// Why: naming where risk concentrates only says anything when more than
    /// one application carries findings — "Acme carries the most RED findings
    /// (1 of 1)" on a single-application report is noise.
    fn apps_with_findings(&self) -> usize {
        self.red_by_app
            .keys()
            .chain(self.amber_by_app.keys())
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    }

    /// The application carrying the most findings, RED before AMBER.
    fn worst_app(&self) -> Option<(&str, usize, &'static str)> {
        if let Some((name, count)) = max_entry(&self.red_by_app) {
            return Some((name, count, "RED"));
        }
        max_entry(&self.amber_by_app).map(|(name, count)| (name, count, "AMBER"))
    }
}

/// The highest-count entry, ties broken by the alphabetically first key so the
/// sentence is stable across runs.
fn max_entry(map: &BTreeMap<String, usize>) -> Option<(&str, usize)> {
    let mut best: Option<(&str, usize)> = None;
    for (name, count) in map {
        if best.is_none_or(|(_, best_count)| *count > best_count) {
            best = Some((name.as_str(), *count));
        }
    }
    best
}

/// Render the paragraph from gathered facts.
///
/// Why: one place composes the sentences, so their order and separators are
/// pinned by a single test rather than assembled at three call sites.
/// What: scope sentence, posture sentence, an optional concentration sentence
/// (only when more than one application carries findings), and [`ROLLUP_NOTE`].
/// Test: `exec_summary_tests.rs::composes_from_metrics_findings`.
fn render(f: &Facts) -> String {
    let mut sentences = vec![scope_sentence(f), posture_sentence(f)];
    if let Some(s) = concentration_sentence(f) {
        sentences.push(s);
    }
    sentences.push(ROLLUP_NOTE.to_string());
    sentences.join(" ")
}

/// "Repository inspection covered 2 applications (A, B), totalling 8200 lines
/// of code across Rust and TypeScript, in 120 files."
fn scope_sentence(f: &Facts) -> String {
    let n = f.app_names.len();
    let noun = if n == 1 {
        "application"
    } else {
        "applications"
    };
    let mut out = format!(
        "Repository inspection covered {n} {noun} ({})",
        name_list(&f.app_names)
    );
    if f.total_loc > 0 {
        out.push_str(&format!(", totalling {} lines of code", f.total_loc));
        let langs = top_languages(f);
        if !langs.is_empty() {
            out.push_str(&format!(" across {}", join_and(&langs)));
        }
    }
    if f.total_files > 0 {
        out.push_str(&format!(", in {} files", f.total_files));
    }
    out.push('.');
    out
}

/// "The analysis raised 1 RED (critical) and 1 AMBER (medium-risk) finding(s),
/// concentrated in security (1) and maintainability (1)."
fn posture_sentence(f: &Facts) -> String {
    if f.red + f.amber == 0 {
        return "The analysis raised no RED or AMBER findings against the data available for \
                this run."
            .to_string();
    }
    let mut out = format!(
        "The analysis raised {} RED (critical) and {} AMBER (medium-risk) findings",
        f.red, f.amber
    );
    let dims = top_dimensions(f);
    if !dims.is_empty() {
        out.push_str(&format!(", concentrated in {}", join_and(&dims)));
    }
    out.push('.');
    out
}

/// "Repro App carries the most RED findings (3 of 5)." — only meaningful when
/// more than one application contributed findings.
fn concentration_sentence(f: &Facts) -> Option<String> {
    if f.apps_with_findings() < 2 {
        return None;
    }
    let (name, count, band) = f.worst_app()?;
    let total = if band == "RED" { f.red } else { f.amber };
    Some(format!(
        "{name} carries the most {band} findings ({count} of {total})."
    ))
}

/// The application names, eliding past [`MAX_APP_NAMES`].
fn name_list(names: &[String]) -> String {
    if names.len() <= MAX_APP_NAMES {
        return names.join(", ");
    }
    format!(
        "{}, and {} more",
        names[..MAX_APP_NAMES].join(", "),
        names.len() - MAX_APP_NAMES
    )
}

/// The largest languages by aggregate LoC, up to [`MAX_LANGUAGES`].
fn top_languages(f: &Facts) -> Vec<String> {
    let mut langs: Vec<(&String, &u64)> =
        f.by_language.iter().filter(|(_, loc)| **loc > 0).collect();
    langs.sort_by_key(|(name, loc)| (std::cmp::Reverse(**loc), (*name).clone()));
    langs
        .into_iter()
        .take(MAX_LANGUAGES)
        .map(|(name, _)| name.clone())
        .collect()
}

/// The most-hit finding dimensions with their counts, up to [`MAX_DIMENSIONS`].
fn top_dimensions(f: &Facts) -> Vec<String> {
    let mut dims: Vec<(&String, &usize)> = f.by_dimension.iter().collect();
    dims.sort_by_key(|(name, count)| (std::cmp::Reverse(**count), (*name).clone()));
    dims.into_iter()
        .take(MAX_DIMENSIONS)
        .map(|(name, count)| format!("{name} ({count})"))
        .collect()
}

/// Join with commas and a final "and".
fn join_and(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [head @ .., last] => format!("{} and {last}", head.join(", ")),
    }
}

#[cfg(test)]
#[path = "exec_summary_tests.rs"]
mod tests;
