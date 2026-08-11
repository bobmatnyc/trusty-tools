//! The credential-redaction boundary every analyze metric crosses on its way
//! into the DD report (#5323).
//!
//! Why: the generated report is acquirer-facing due-diligence output about
//! someone else's codebase, so a credential this process holds must never reach
//! it. tga's report path has had that guarantee since #5239 — `build_dd_manifest`
//! scrubs every string it emits — while trusty-review's analyze path had none:
//! `MetricFinding.description` and `.remediation` were copied verbatim from the
//! analyze daemon, from a declared metrics JSON, and from the investigation twin
//! straight into template fields. Nothing leaked today only because the upstream
//! generators happen not to quote raw source text; a richer linter message format
//! would have turned that into a disclosure with no gate to catch it. This module
//! makes the property an enforced boundary rather than an upstream accident.
//!
//! What: [`report_secrets`] resolves the needle set; [`scrub_metrics`] applies it
//! to every string an external producer supplied in an [`AnalyzeMetrics`] and
//! [`scrub_investigation`] does the same for an [`Investigation`] — not just the
//! two prose slots the ticket named, since the daemon authors the bucket labels,
//! language names, and finding titles too. The scrub runs where findings ENTER a
//! report sink, ahead of every consumer: the rendered finding bands, the
//! executive summary, the synthesis digest that is sent to an LLM provider, and
//! the JSON twin. Running it at ingest is also what keeps the #5239 ordering
//! lesson satisfied — `scrub_secrets` is an exact full-value substring match, so
//! a credential split by a later truncation (`dedupe_field`, the benchmark's
//! basename trim) would survive as an unmatched fragment if the scrub ran after.
//!
//! **Two sinks, not one.** Scrubbing `AnalyzeMetrics` alone is not enough, and
//! that gap shipped in this ticket's first round. An investigation finding
//! reaches the page by TWO independent routes: `apply_investigation` derives a
//! `MetricFinding` (the metrics sink), and `merge_investigation_prose` builds a
//! `FindingProse` on `Synthesis` (the synthesis sink). `FindingRow::merge_prose`
//! then overwrites the metrics-derived prose with the synthesis one
//! unconditionally, so a scrub applied only to the first is discarded before
//! render on every run that produces a RED/AMBER finding. `evidence_quote` is
//! synthesis-only and renders byte-for-byte verbatim, so it has no metrics route
//! at all. Both entry points scrub, and neither is redundant with the other.
//!
//! **What this cannot do.** It removes only values this process can resolve. A
//! credential the target repository holds, one a linter read out of its own
//! config, or one rotated since resolution passes through untouched — see
//! [`trusty_common::credentials::scrub_secrets`] for the full limit. Scrubbed
//! text is lower-risk, not proven secret-free.
//!
//! Test: `redact_tests.rs` — the pure scrub, one wiring test per producer
//! (`enrich_scrubs_configured_credentials_from_findings`,
//! `apply_investigation_scrubs_configured_credentials`,
//! `declared_metrics_file_findings_are_scrubbed`), and
//! `investigation_credentials_never_reach_the_rendered_report`, which asserts on
//! the rendered markdown and the JSON twin rather than on an intermediate.

use trusty_common::credentials::{resolved_secret_values, scrub_secrets};

use super::investigate::{Investigation, InvestigationStatus};
use super::metrics::{AnalyzeMetrics, MetricFinding};
use super::synthesize::FindingProse;

/// Scrub one string in place, skipping the empty case.
fn scrub_in_place(field: &mut String, secrets: &[String]) {
    if !field.is_empty() {
        *field = scrub_secrets(field, secrets);
    }
}

/// Every credential this process can resolve, as scrub needles.
///
/// Why: the ticket's open question was how trusty-review obtains the needle set,
/// since it never receives the tga config `configured_secrets` reads and passing
/// secrets by argv (visible to `ps`) would be worse than the gap. It does not
/// need to: `resolved_secret_values` walks the same provider registry and the
/// same env > `.env.local` > store precedence every consumer uses, in-process,
/// and its registry covers the forge/tracker/inference credentials tga's config
/// holds (`GITHUB_TOKEN`, `GH_TOKEN`, `JIRA_TOKEN`, `LINEAR_API_KEY`,
/// `BITBUCKET_TOKEN`, `OPENROUTER_API_KEY`, …). Deriving a second needle set here
/// would be exactly the drift the common-entry-point rule forbids.
/// What: a thin pass-through, named so call sites read as report-domain code and
/// so the one place trusty-review materialises raw credentials is greppable.
/// Returns raw secrets: the ONLY correct use is as a [`scrub_metrics`] needle
/// set — never log, serialise, or render the result. Resolution touches the
/// filesystem (and, where compiled in, the keychain), so callers resolve once per
/// pipeline stage and reuse the slice rather than once per repository.
/// Test: `redact_tests.rs::report_secrets_yields_a_usable_needle_set`.
pub fn report_secrets() -> Vec<String> {
    resolved_secret_values()
}

/// Remove every needle in `secrets` from one finding's producer-supplied strings.
///
/// Why: `description` and `remediation` are the two the ticket named, but they
/// are not the only externally-authored fields — `title` carries a linter's rule
/// code, `category` its tool name, and `component` a path, all copied verbatim
/// from the daemon's wire JSON. Scrubbing the two while leaving three next to
/// them would close one instance of the defect and not its shape.
/// What: applies [`scrub_secrets`] to all five strings, skipping empty ones. A
/// no-op when `secrets` is empty — a process holding no resolvable credential has
/// nothing to remove.
/// Test: `redact_tests.rs::scrub_finding_covers_every_producer_supplied_field`.
pub fn scrub_finding(finding: &mut MetricFinding, secrets: &[String]) {
    if secrets.is_empty() {
        return;
    }
    for field in [
        &mut finding.title,
        &mut finding.category,
        &mut finding.component,
        &mut finding.description,
        &mut finding.remediation,
    ] {
        scrub_in_place(field, secrets);
    }
}

/// Remove every needle in `secrets` from a whole metrics document.
///
/// Why: this is the boundary call — every producer that puts an
/// [`AnalyzeMetrics`] on the model routes through it, so a fix or a widening
/// lands once instead of per producer.
/// What: scrubs the repository label, `schema_version`, each language name, each
/// complexity-bucket label, and every finding via [`scrub_finding`]. Numeric
/// fields cannot carry a credential and are left alone. A no-op when `secrets` is
/// empty.
///
/// `schema_version` looks like a constant and is one on the daemon path
/// (`analyze_adapter.rs` hardcodes it), but a manifest-declared metrics JSON
/// deserialises it from an arbitrary externally-authored file with no content
/// constraint and it reaches the JSON twin verbatim.
/// Test: `redact_tests.rs::scrub_metrics_reaches_every_string_field`.
pub fn scrub_metrics(metrics: &mut AnalyzeMetrics, secrets: &[String]) {
    if secrets.is_empty() {
        return;
    }
    scrub_in_place(&mut metrics.repository, secrets);
    scrub_in_place(&mut metrics.schema_version, secrets);
    for lang in &mut metrics.loc.by_language {
        scrub_in_place(&mut lang.language, secrets);
    }
    for bucket in &mut metrics.complexity.buckets {
        scrub_in_place(&mut bucket.label, secrets);
    }
    for finding in &mut metrics.findings {
        scrub_finding(finding, secrets);
    }
}

/// Remove every needle in `secrets` from one synthesis finding's prose.
///
/// Why: this is the second sink, and the one the ticket's first round missed.
/// `FindingRow::merge_prose` overwrites the metrics-derived prose with these
/// fields unconditionally, so scrubbing the metrics route alone is discarded
/// before render. `evidence` never has a metrics route at all — `raw_evidence`
/// renders it byte-for-byte verbatim inside a fenced block.
/// What: scrubs all seven strings, leaving `severity`, `app_slug`, and the
/// `evidence_measured` flag alone — a band label and a slug are report-authored,
/// not producer-authored. A no-op when `secrets` is empty.
///
/// Scrubbing `evidence` does alter a quote documented as verbatim. That is the
/// intended trade: a quote is verbatim so a reader can match it against the
/// source, and a credential is the one substring that must not be matchable.
/// Test: `redact_tests.rs::scrub_prose_covers_evidence_and_every_narrative_field`.
pub fn scrub_prose(prose: &mut FindingProse, secrets: &[String]) {
    if secrets.is_empty() {
        return;
    }
    for field in [
        &mut prose.title,
        &mut prose.description,
        &mut prose.evidence,
        &mut prose.component,
        &mut prose.business_impact,
        &mut prose.remediation,
        &mut prose.cost_effort,
    ] {
        scrub_in_place(field, secrets);
    }
}

/// Remove every needle in `secrets` from a whole investigation record.
///
/// Why: `apply_investigation` clones the `Investigation` onto the model, where
/// `reporter.rs` serialises it into the JSON twin and `investigate::render`
/// emits the dependency-inventory and coverage sections. Scrubbing only the
/// findings derived from it would leave the record itself — including a failure
/// reason that can quote a provider's error body — unredacted in the artifact.
/// What: walks every string in the tree: repository slug/name, the status reason,
/// each verified finding's eight text fields, the covered/absent dimension lists,
/// each failed batch's reason and file list, and the dependency rows. Counts,
/// byte totals, and severity bands are not text and are left alone. A no-op when
/// `secrets` is empty.
/// Test: `redact_tests.rs::scrub_investigation_reaches_the_whole_tree`.
pub fn scrub_investigation(inv: &mut Investigation, secrets: &[String]) {
    if secrets.is_empty() {
        return;
    }
    for repo in &mut inv.repos {
        scrub_in_place(&mut repo.slug, secrets);
        scrub_in_place(&mut repo.name, secrets);
        match &mut repo.status {
            InvestigationStatus::Skipped(reason) | InvestigationStatus::Unavailable(reason) => {
                scrub_in_place(reason, secrets);
            }
            InvestigationStatus::Available => {}
        }
        for f in &mut repo.findings {
            for field in [
                &mut f.title,
                &mut f.dimension,
                &mut f.file,
                &mut f.evidence_quote,
                &mut f.description,
                &mut f.business_impact,
                &mut f.remediation,
                &mut f.cost_effort,
            ] {
                scrub_in_place(field, secrets);
            }
        }
        for d in repo
            .coverage
            .dimensions_covered
            .iter_mut()
            .chain(repo.coverage.dimensions_absent.iter_mut())
        {
            scrub_in_place(d, secrets);
        }
        for note in &mut repo.coverage.batches_failed {
            scrub_in_place(&mut note.reason, secrets);
            for file in &mut note.files {
                scrub_in_place(file, secrets);
            }
        }
        for dep in &mut repo.deps.deps {
            scrub_in_place(&mut dep.name, secrets);
            scrub_in_place(&mut dep.ecosystem, secrets);
            scrub_in_place(&mut dep.spec, secrets);
            if let Some(locked) = dep.locked.as_mut() {
                scrub_in_place(locked, secrets);
            }
        }
    }
}

#[cfg(test)]
#[path = "redact_tests.rs"]
mod tests;
