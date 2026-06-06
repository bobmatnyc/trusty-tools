//! `workflow-report.md` parsing helpers.
//!
//! Why: The workflow engine writes a free-form markdown report; the supervisor
//! needs a single tolerant parser that extracts the outcome label, summary,
//! next-steps, and completed-work bullets without spawning a real workflow so
//! the logic stays unit-testable.
//! What: `parse_workflow_report` plus the private section/bullet/outcome
//! extraction helpers it composes.
//! Test: `supervisor/tests.rs` (`test_parse_workflow_report_*`,
//! `detect_outcome_falls_back_to_inline_partial`,
//! `extract_section_returns_none_for_missing_heading`).

use super::types::{ReportSummary, WorkflowOutcome};

/// Parse a `workflow-report.md` into a structured outcome + summary + next-steps.
///
/// Why: The workflow engine writes a free-form markdown report whose authoritative
/// outcome label appears either as `**Final Verdict**`, a `**Status**:` field, or
/// inline near the start (e.g. "classified as **partial**"). We need a single
/// tolerant parser that picks the strongest signal it can find.
/// What: Walks the report text, prefers explicit structured markers (`Final Verdict`
/// or `Status`), then falls back to scanning for `success` / `partial` / `fail` in
/// the body. Captures the `## Summary` body (or first non-heading paragraph as a
/// fallback) and the `## Next Steps` section verbatim if present.
/// Test: `test_parse_workflow_report_success`, `test_parse_workflow_report_partial`.
pub fn parse_workflow_report(content: &str) -> ReportSummary {
    let outcome = detect_outcome(content);
    let summary = extract_section(content, "Summary")
        .or_else(|| extract_section(content, "Final Verdict"))
        .unwrap_or_else(|| {
            content
                .lines()
                .find(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
                .unwrap_or("")
                .trim()
                .to_string()
        });
    let next_steps = extract_section(content, "Next Steps").and_then(|s| {
        let trimmed = s.trim();
        // The example reports use "*(none)*" to mean no next steps; suppress it.
        if trimmed.is_empty() || trimmed == "*(none)*" || trimmed.eq_ignore_ascii_case("none") {
            None
        } else {
            Some(trimmed.to_string())
        }
    });

    let completed_work = extract_section(content, "Completed Work")
        .or_else(|| extract_section(content, "Completed"))
        .map(|s| extract_bullets(&s))
        .unwrap_or_default();

    let bullets: Vec<String> = next_steps
        .as_deref()
        .map(extract_bullets)
        .unwrap_or_default();

    let mut in_scope = Vec::new();
    let mut out_of_scope = Vec::new();
    for b in bullets {
        if is_out_of_scope_step(&b) {
            out_of_scope.push(b);
        } else {
            in_scope.push(b);
        }
    }

    ReportSummary {
        outcome,
        summary,
        next_steps,
        completed_work,
        in_scope_next_steps: in_scope,
        out_of_scope_items: out_of_scope,
    }
}

/// Split a free-form markdown blob into bullet items (one per line).
///
/// Why: Both `## Next Steps` and `## Completed` are typically markdown bullet
/// lists. We need each item on its own to apply per-item heuristics. Lines that
/// don't look like bullets are accepted as a single item so we don't drop
/// content from prose-style sections.
/// What: For each line, strips leading `-`, `*`, `+`, or `1.` markers and
/// trims; empty lines are skipped. If no bullets are found, returns the trimmed
/// blob as a single item (when non-empty).
/// Test: Indirect via `test_parse_workflow_report_segregates_next_steps`.
fn extract_bullets(blob: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut had_any_bullet = false;
    for line in blob.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let stripped = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
            .or_else(|| trimmed.strip_prefix("+ "))
            .map(|s| s.to_string());
        if let Some(s) = stripped {
            had_any_bullet = true;
            out.push(s);
            continue;
        }
        // Numbered list ("1. foo")
        if let Some(rest) = strip_numbered_prefix(trimmed) {
            had_any_bullet = true;
            out.push(rest);
            continue;
        }
        // Continuation / prose line — append to last bullet if there is one.
        if had_any_bullet {
            if let Some(last) = out.last_mut() {
                last.push(' ');
                last.push_str(trimmed);
            }
        } else {
            out.push(trimmed.to_string());
        }
    }
    out
}

/// Strip a leading "1. " / "12. " marker from a line, returning the body if
/// matched.
fn strip_numbered_prefix(s: &str) -> Option<String> {
    let mut idx = 0;
    let bytes = s.as_bytes();
    while idx < bytes.len() && bytes[idx].is_ascii_digit() {
        idx += 1;
    }
    if idx == 0 || idx >= bytes.len() {
        return None;
    }
    if bytes[idx] == b'.' && idx + 1 < bytes.len() && bytes[idx + 1] == b' ' {
        return Some(s[idx + 2..].to_string());
    }
    None
}

/// Heuristic: does a next-steps item describe pre-existing / unrelated work?
///
/// Why: When the QA phase reports failures we didn't introduce (e.g. an
/// existing i18n translation gap, a flaky test, work tracked under a different
/// ticket), the supervisor should not loop forever trying to fix them. We need
/// a cheap signal to flag those items as out of scope.
/// What: Lowercases the item and checks for any of a small set of phrases that
/// strongly indicate pre-existing / out-of-scope failures.
/// Test: Indirect via `test_parse_workflow_report_segregates_next_steps` and
/// `test_classify_qa_preexisting_failures`.
fn is_out_of_scope_step(item: &str) -> bool {
    let l = item.to_ascii_lowercase();
    const NEEDLES: &[&str] = &[
        "pre-existing",
        "preexisting",
        "pre existing",
        "unrelated",
        "out of scope",
        "out-of-scope",
        "i18n",
        "translation",
        "existing test",
        "existing failure",
        "existed before",
        "before this",
        "prior to this",
        "not introduced by",
        "not caused by this",
        "flaky",
    ];
    NEEDLES.iter().any(|n| l.contains(n))
}

/// Determine the workflow outcome label from a markdown report.
///
/// Why: Outcome detection is the load-bearing decision; isolating it makes the
/// fallback chain explicit and unit-testable.
/// What: 1) explicit `Final Verdict` section content; 2) `**Status**:` line;
/// 3) any `**success**` / `**partial**` / `**fail**` bold marker in the body;
/// 4) bare keywords. Defaults to `Fail` so the supervisor errs on the side of
/// retry / escalate when the report is unparseable.
/// Test: Covered by both report-parsing tests.
pub(super) fn detect_outcome(content: &str) -> WorkflowOutcome {
    if let Some(verdict) = extract_section(content, "Final Verdict")
        && let Some(o) = match_outcome(&verdict)
    {
        return o;
    }
    for line in content.lines() {
        let lower = line.to_ascii_lowercase();
        if (lower.contains("**status**") || lower.starts_with("status:"))
            && let Some(o) = match_outcome(line)
        {
            return o;
        }
    }
    if let Some(o) = match_outcome(content) {
        return o;
    }
    WorkflowOutcome::Fail
}

/// Match a substring against the three outcome keywords with priority
/// success > partial > fail.
///
/// Why: Reports often mention multiple outcomes ("partial: code phase failed"),
/// so simple `.contains` ordering matters.
/// What: Lowercases once, then checks for whole-word-ish hits. Returns None if
/// no keyword is found.
/// Test: Indirect via `detect_outcome`.
fn match_outcome(s: &str) -> Option<WorkflowOutcome> {
    let l = s.to_ascii_lowercase();
    if l.contains("success") {
        Some(WorkflowOutcome::Success)
    } else if l.contains("partial") {
        Some(WorkflowOutcome::Partial)
    } else if l.contains("fail") {
        Some(WorkflowOutcome::Fail)
    } else {
        None
    }
}

/// Extract the body of a `## <heading>` section from a markdown document.
///
/// Why: Both `## Summary` and `## Next Steps` are sliced the same way; one
/// helper avoids duplicating the line-walk.
/// What: Case-insensitive heading match (allowing `#`, `##`, `###`). Returns
/// everything until the next heading at the same-or-shallower depth, trimmed.
/// Test: Indirect via the report-parsing tests.
pub(super) fn extract_section(content: &str, heading: &str) -> Option<String> {
    let needle = heading.to_ascii_lowercase();
    let lines: Vec<&str> = content.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let l = lines[i].trim_start();
        if l.starts_with('#') {
            let title = l.trim_start_matches('#').trim().to_ascii_lowercase();
            if title == needle {
                let mut out = Vec::new();
                let mut j = i + 1;
                while j < lines.len() {
                    let nl = lines[j].trim_start();
                    if nl.starts_with('#') {
                        break;
                    }
                    out.push(lines[j]);
                    j += 1;
                }
                return Some(out.join("\n").trim().to_string());
            }
        }
        i += 1;
    }
    None
}
