//! The guardrail layer over one parsed synthesis response.
//!
//! Why: `synthesize.rs` crossed the 500-SLOC production cap when #6082 lap 8
//! reworked the status notes. This is the natural seam — the numeric guardrail
//! and the grounding guardrail are one unit that takes a [`RawSynthesis`] and
//! returns the [`Synthesis`] that may ship, and nothing above it does anything
//! but call [`apply_guardrail`].
//! What: [`apply_guardrail`], [`ground_investigation_prose`], the per-field
//! `admit`/`ground` pair both go through, and the reader-facing note each
//! rejection records.
//! Test: `synthesize_tests.rs`.

use super::model::ReportModel;
use super::synthesize::{RawSynthesis, StatusNote, Synthesis, SynthesisError};
use crate::report::synthesize_grounding::{Grounding, GroundingOutcome};
use crate::report::synthesize_guard::verify_prose;

/// One reader-facing disclosure that a figure the model wrote is not in the data.
///
/// #6082 lap 8 replaced the former `synthesis: rejected (unverified figure)` log
/// prefix. Half the Synthesis Status block was already reader prose and half was
/// this, so the same block told a diligence reader two different things about
/// how much of it was written for them.
fn unverified_figure_note(field: &str, subject: Option<&str>, token: &str) -> StatusNote {
    let text = format!(
        "the model's {field} was withheld because it carried the figure {token}, which is not \
         in the collected data"
    );
    match subject {
        Some(s) => StatusNote::about(s, text),
        None => StatusNote::plain(text),
    }
    .withheld()
}

/// One reader-facing disclosure that a CLAIM the model wrote contradicts the
/// report's own data (#6082 lap 4).
///
/// Kept distinct from [`unverified_figure_note`]: a numeric rejection means a
/// figure was not in the data, a grounding rejection means a claim contradicted
/// it, and a reader who sees one should not have to guess which happened.
///
/// `subject` is what the reporter resolves to a §5.1/§5.2 number. For a
/// finding's own field it is that finding; for report-level prose it is the
/// finding the grounding check identified the refused claim as being about
/// (#6082 lap 9) — without which the line quoted a title the reader then had to
/// search nine hundred lines of document to find.
fn ungrounded_claim_note(field: &str, subject: Option<&str>, reason: &str) -> StatusNote {
    let text = format!("the model's {field} was withheld — {reason}");
    match subject {
        Some(s) => StatusNote::about(s, text),
        None => StatusNote::plain(text),
    }
    .withheld()
}

/// What one narrative field is, for the disclosure that names it.
///
/// Why: the guardrails record a note per field, and the note has to say which
/// field of which finding — a bundle keeps the two halves together instead of
/// growing another positional argument on every call (#6082 lap 8).
/// What: the reader-facing field name, the §5.2 finding it belongs to (absent
/// for report-level prose), and that finding's component, which decides its
/// reachability tier in [`Grounding::check_field`].
struct FieldCtx<'a> {
    field: &'a str,
    subject: Option<&'a str>,
    component: Option<&'a str>,
}

impl<'a> FieldCtx<'a> {
    /// Report-level prose belonging to no single finding.
    fn report(field: &'a str) -> Self {
        FieldCtx {
            field,
            subject: None,
            component: None,
        }
    }

    /// One field of one finding, carrying the component its tier is read from.
    fn finding(field: &'a str, title: &'a str, component: &'a str) -> Self {
        FieldCtx {
            field,
            subject: Some(title),
            component: Some(component),
        }
    }
}

/// Apply the numeric guardrail to the raw synthesis, field by field.
///
/// Why: a field whose prose cites a figure not in the source is dropped (never
/// repaired), so the deterministic composition (#5374) fills that placeholder and
/// a visible rejection note is recorded.  Per-FIELD rejection stays a correctness
/// property under #5454's required-inference rule; what changed is that a
/// response with nothing left after the pass is an error rather than a
/// deterministic-only report.
/// What: verifies the executive summary and every risk row / finding against
/// `allowed`; keeps only clean fields.  Findings must carry a RED/AMBER severity
/// (defence-in-depth greens exclusion).  `seed_notes` (the normalization
/// drop-notes recorded by [`crate::report::synthesize_normalize`], #6009 shape
/// 3) are recorded first, so an unrecognized-field drop is visible in the
/// rendered status lines alongside a numeric-guardrail rejection.  `Ok` iff at
/// least one field survived.
///
/// # Errors
///
/// [`SynthesisError::NoVerifiableContent`] when every field was rejected.
///
/// Test: `synthesize_tests.rs::{synthesize_rejects_unverified_figure,
/// synthesize_happy_path_injects}`.
pub(super) fn apply_guardrail(
    raw: RawSynthesis,
    allowed: &std::collections::HashSet<String>,
    seed_notes: Vec<StatusNote>,
    grounding: &Grounding,
) -> Result<Synthesis, SynthesisError> {
    let mut out = Synthesis {
        notes: seed_notes,
        ..Synthesis::default()
    };
    let notes = &mut out.notes;

    // Executive summary.
    out.executive_summary = admit_field(
        &FieldCtx::report("executive summary"),
        &raw.executive_summary,
        allowed,
        grounding,
        notes,
    );

    // #6004: Code Quality & Architecture and Security Posture narratives — same
    // per-field guardrail path as the executive summary.
    out.code_quality_summary = admit_field(
        &FieldCtx::report("code quality summary"),
        &raw.code_quality_summary,
        allowed,
        grounding,
        notes,
    );
    out.security_summary = admit_field(
        &FieldCtx::report("security summary"),
        &raw.security_summary,
        allowed,
        grounding,
        notes,
    );
    out.authorship_summary = admit_field(
        &FieldCtx::report("authorship summary"),
        &raw.authorship_summary,
        allowed,
        grounding,
        notes,
    );

    // Top-risk rows. The row's description is narrative and takes the same
    // grounding pass the paragraphs do (#6082 lap 4) — row 1 of the last report
    // carried the same reachability contradiction the executive summary did.
    for (i, mut r) in raw.top_risks.into_iter().enumerate() {
        let field = format!("description for top risk {}", i + 1);
        let ctx = FieldCtx::report(&field);
        let Some(description) = admit_field(&ctx, &r.description, allowed, grounding, notes) else {
            continue;
        };
        r.description = description;
        match verify_all(&[&r.severity, &r.cost, &r.apps], allowed) {
            Ok(()) => out.top_risks.push(r),
            Err(tok) => notes.push(unverified_figure_note(&field, None, &tok)),
        }
    }

    // RED/AMBER finding elaborations.
    for mut f in raw.findings {
        let sev = f.severity.to_uppercase();
        if sev != "RED" && sev != "AMBER" {
            notes.push(StatusNote::about(
                &f.title,
                format!(
                    "the model's narrative was dropped because it declared severity '{}', and \
                     only RED and AMBER findings carry one",
                    f.severity
                ),
            ));
            continue;
        }
        // #6082 lap 5: a finding's OWN narrative fields take the same grounding
        // pass the paragraphs do. Wiring the guard to the summaries and the
        // top-risk rows alone left RED finding 3's business impact stating
        // "remote/local code execution" two lines from a Security Posture
        // paragraph the same guard had corrected to "not remotely".
        // `evidence` is excluded deliberately: it is a verbatim quote verified
        // against the file, so rewriting it would make the displayed quote
        // diverge from the one that was checked. `component` is routing data,
        // not narrative — and since #6082 lap 8 it is also what decides the
        // finding's reachability tier.
        let (title, component) = (f.title.clone(), f.component.clone());
        for (field, value) in [
            ("description", &mut f.description),
            ("business impact", &mut f.business_impact),
            ("remediation", &mut f.remediation),
            ("cost/effort", &mut f.cost_effort),
        ] {
            *value = ground_field(
                &FieldCtx::finding(field, &title, &component),
                value,
                grounding,
                notes,
            );
        }
        match verify_all(
            &[
                &f.description,
                &f.evidence,
                &f.component,
                &f.business_impact,
                &f.remediation,
                &f.cost_effort,
            ],
            allowed,
        ) {
            Ok(()) => out.findings.push(f),
            Err(tok) => notes.push(unverified_figure_note("narrative", Some(&f.title), &tok)),
        }
    }

    if out.executive_summary.is_none()
        && out.code_quality_summary.is_none()
        && out.security_summary.is_none()
        && out.authorship_summary.is_none()
        && out.top_risks.is_empty()
        && out.findings.is_empty()
    {
        return Err(SynthesisError::NoVerifiableContent);
    }
    Ok(out)
}

/// Run the grounding guardrail over the verified investigation's own prose.
///
/// Why: the guard used to run on synthesis prose only, and
/// `merge_investigation_prose` overwrites that prose with the investigation's
/// for every RED/AMBER finding — so the guarded copy was discarded before
/// render. That is how RED finding 2 of the lap-6 report shipped "a remote code
/// execution risk" in its business impact while the guard's own note in the same
/// document recorded a correction it had made elsewhere. Correcting `inv` BEFORE
/// `apply_investigation` fixes every downstream copy at once: the injected
/// `metrics.findings` rows, `model.investigation` in the JSON twin, the
/// synthesis prompt, and the merged prose that renders.
/// What: builds the [`Grounding`] from the model plus `inv` (the model has not
/// recorded it yet), then runs [`ground_field`] over each RED/AMBER finding's
/// description, business impact, remediation and cost/effort. `evidence_quote`
/// is excluded for the same reason it is in [`apply_guardrail`]: it is verified
/// verbatim against the file. Returns one status line per correction or
/// rejection, for the caller to fold into [`Synthesis::notes`].
/// Test: `synthesize_tests.rs::{investigation_business_impact_states_host_local_reach,
/// investigation_grounding_leaves_a_clean_finding_alone}`.
pub fn ground_investigation_prose(
    model: &ReportModel,
    inv: &mut crate::report::investigate::Investigation,
) -> Vec<StatusNote> {
    let grounding = Grounding::from_model_with(model, Some(inv));
    let mut notes = Vec::new();
    for repo in &mut inv.repos {
        for f in &mut repo.findings {
            if f.severity == crate::report::metrics::Severity::Green {
                continue; // greens are title-only topics; they carry no narrative
            }
            let (title, file) = (f.title.clone(), f.file.clone());
            for (field, value) in [
                ("description", &mut f.description),
                ("business impact", &mut f.business_impact),
                ("remediation", &mut f.remediation),
                ("cost/effort", &mut f.cost_effort),
            ] {
                *value = ground_field(
                    &FieldCtx::finding(field, &title, &file),
                    value,
                    &grounding,
                    &mut notes,
                );
            }
        }
    }
    notes
}

/// Run the grounding guardrail alone over one narrative field of a finding.
///
/// Why: a finding is verified as a WHOLE by [`verify_all`] — one bad figure
/// anywhere rejects the entire finding — so its fields cannot go through
/// [`admit_field`], which rejects per field. This is the grounding half on its
/// own, leaving that all-or-nothing numeric contract untouched (#6082 lap 5).
/// What: returns the text with any reachability claim corrected, or an empty
/// string when the claim could not be corrected safely. An emptied field falls
/// through to the reporter's honesty marker, which is what an absent field has
/// always rendered as.
/// Test: `synthesize_tests.rs::{synthesize_grounds_a_finding_business_impact,
/// synthesize_empties_an_uncorrectable_remote_claim_in_a_finding}`.
fn ground_field(
    ctx: &FieldCtx<'_>,
    raw: &str,
    grounding: &Grounding,
    notes: &mut Vec<StatusNote>,
) -> String {
    let text = raw.trim();
    if text.is_empty() {
        return String::new();
    }
    match grounding.check_field(text, ctx.component, ctx.subject) {
        GroundingOutcome::Clean => text.to_string(),
        GroundingOutcome::Rewritten(fixed, corrections) => {
            notes.extend(corrections.into_iter().map(|c| note_for(ctx, &c)));
            fixed
        }
        GroundingOutcome::Rejected { reason, subject } => {
            let about = ctx.subject.or(subject.as_deref());
            notes.push(ungrounded_claim_note(ctx.field, about, &reason));
            String::new()
        }
    }
}

/// Attach one grounding correction to the finding whose field carried it.
///
/// #6082 lap 10: report-level prose has no `ctx.subject`, so a correction made
/// in it used to ship with no subject at all and cited its finding by title
/// while the withheld lines beside it carried a §5.1/§5.2 number. The grounding
/// check knows which finding it corrected — fall back to that, exactly as
/// [`ground_field`] already does for a rejection.
fn note_for(ctx: &FieldCtx<'_>, c: &crate::report::synthesize_grounding::Correction) -> StatusNote {
    match ctx.subject.or(c.subject.as_deref()) {
        Some(s) => StatusNote::about(s, c.text.clone()),
        None => StatusNote::plain(c.text.clone()),
    }
}

/// Run both guardrails over one narrative field and return what may ship.
///
/// Why: the grounding check (#6082 lap 4) and the numeric check are two
/// independent reasons a field cannot be trusted, and running them from one
/// place is what stops a future field from being wired to only one of them.
/// What: `None` for an empty field, for a grounding rejection, and for a numeric
/// rejection — each of the last two recording its own note under its own prefix.
/// Otherwise the text to keep, which is the model's own unless the grounding
/// pass corrected a claim in it; the numeric check then runs on the CORRECTED
/// text, so a rewrite can never smuggle a figure past it.
/// Test: `synthesize_tests.rs::{synthesize_rewrites_a_remote_claim_about_a_local_finding,
/// synthesize_rejects_a_load_bearing_claim_about_a_leaf_crate}`.
fn admit_field(
    ctx: &FieldCtx<'_>,
    raw: &str,
    allowed: &std::collections::HashSet<String>,
    grounding: &Grounding,
    notes: &mut Vec<StatusNote>,
) -> Option<String> {
    let text = raw.trim();
    if text.is_empty() {
        return None;
    }
    let grounded = match grounding.check_field(text, ctx.component, ctx.subject) {
        GroundingOutcome::Clean => text.to_string(),
        GroundingOutcome::Rewritten(fixed, corrections) => {
            notes.extend(corrections.into_iter().map(|c| note_for(ctx, &c)));
            fixed
        }
        GroundingOutcome::Rejected { reason, subject } => {
            let about = ctx.subject.or(subject.as_deref());
            notes.push(ungrounded_claim_note(ctx.field, about, &reason));
            return None;
        }
    };
    match verify_prose(&grounded, allowed) {
        Ok(()) => Some(grounded),
        Err(tok) => {
            notes.push(unverified_figure_note(ctx.field, ctx.subject, &tok));
            None
        }
    }
}

/// Verify every field of a group; returns the first offending token on failure.
fn verify_all(fields: &[&str], allowed: &std::collections::HashSet<String>) -> Result<(), String> {
    for field in fields {
        verify_prose(field, allowed)?;
    }
    Ok(())
}
