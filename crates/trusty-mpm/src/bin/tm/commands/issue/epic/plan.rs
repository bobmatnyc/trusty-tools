//! The plan-document parser (#8447).
//!
//! Why: a tracker and its phase issues are derived from one committed document,
//! so that document's headings ARE the schema. A missing heading has to be a
//! named refusal rather than an empty section, because an empty section files
//! a tracker that looks complete and is not — and the one heading whose
//! emptiness is itself a refusal is `### Ordering`, since the pattern's own
//! gate test says work with nothing to say about ordering does not need an
//! epic at all.
//! What: [`parse`] reads the `## Epic plan` region of a markdown document into
//! an [`EpicPlan`] — summary, outcomes, ratified decisions, ordering, one
//! [`PhasePlan`] per `### Phase:` heading, and an optional `### Deferred`
//! table. Every refusal is a [`PlanError`] variant naming the heading or the
//! phase at fault. The document's own `phases` marker block, if it carries one,
//! is stripped from the ordering prose: it is the document's copy, superseded
//! by the tracker's (D5).
//! Test: `plan_parses_the_committed_epic_plan`,
//! `plan_refuses_a_document_with_no_epic_plan_heading`,
//! `plan_refuses_an_empty_ordering_section`,
//! `plan_refuses_a_missing_ordering_section`,
//! `plan_refuses_a_phase_with_no_gate_line`,
//! `plan_refuses_a_phase_with_no_acceptance_criteria`,
//! `plan_refuses_a_document_with_no_phases`,
//! `plan_strips_the_documents_own_phases_block`.

use crate::commands::issue::epic::render::{
    DEFERRED_END, DEFERRED_START, FOLLOWUPS_END, FOLLOWUPS_START, PHASES_END, PHASES_START,
};

/// The `## Epic plan` section root.
pub(crate) const EPIC_PLAN_HEADING: &str = "## Epic plan";
/// The outcomes subsection.
const OUTCOMES_HEADING: &str = "### Outcomes";
/// The ratified-decisions subsection.
const DECISIONS_HEADING: &str = "### Ratified decisions";
/// The ordering subsection, whose emptiness is itself a refusal.
const ORDERING_HEADING: &str = "### Ordering";
/// The prefix marking a phase subsection.
const PHASE_PREFIX: &str = "### Phase:";
/// The optional deferred subsection.
const DEFERRED_HEADING: &str = "### Deferred";
/// The acceptance-criteria subsection inside a phase.
const ACCEPTANCE_HEADING: &str = "#### Acceptance criteria";
/// The `Gate:` line every phase declares.
const GATE_PREFIX: &str = "Gate:";

/// Why a plan document cannot be turned into an epic.
///
/// Why: "the parse failed" is not actionable; the author needs the heading to
/// add or the phase to fix. Each variant names exactly one.
/// What: one variant per schema rule, carrying the document path and the
/// offending heading or phase title.
/// Test: the `plan_refuses_*` tests.
#[derive(Debug, thiserror::Error)]
pub(crate) enum PlanError {
    /// A required heading is absent.
    #[error("{path} declares no `{heading}` heading — the plan document's schema requires it")]
    MissingHeading {
        /// Document path, as the operator named it.
        path: String,
        /// The heading that is missing.
        heading: &'static str,
    },
    /// `### Ordering` exists but says nothing.
    #[error(
        "{path}'s `### Ordering` section is empty — an epic needs a reason its phases run in \
         order. Work with nothing to say here does not need an epic; file one issue with a task \
         list instead"
    )]
    EmptyOrdering {
        /// Document path, as the operator named it.
        path: String,
    },
    /// The document declares no phase at all.
    #[error("{path} declares no `### Phase: <title>` section — an epic needs at least one phase")]
    NoPhases {
        /// Document path, as the operator named it.
        path: String,
    },
    /// A phase is missing its `Gate:` line.
    #[error(
        "{path}'s `### Phase: {phase}` has no `Gate:` line — the gate is what justifies using \
         the tracker pattern at all"
    )]
    MissingGate {
        /// Document path, as the operator named it.
        path: String,
        /// The phase title at fault.
        phase: String,
    },
    /// A phase is missing its acceptance-criteria list.
    #[error("{path}'s `### Phase: {phase}` has no `{ACCEPTANCE_HEADING}` list")]
    MissingAcceptance {
        /// Document path, as the operator named it.
        path: String,
        /// The phase title at fault.
        phase: String,
    },
}

/// One phase, as the plan document declares it.
///
/// Why: a phase becomes one issue, so everything that issue needs — its title,
/// its gate, and its body — is resolved at parse time rather than being
/// re-derived by the filing code.
/// What: the title from the `### Phase:` heading, the `Gate:` line's text, and
/// the remaining section lines with their `####` headings promoted to `##` so
/// they read as top-level sections in the issue body.
/// Test: `plan_parses_the_committed_epic_plan`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PhasePlan {
    /// What this phase does, from its heading.
    pub(crate) title: String,
    /// The gate that must hold before this phase starts.
    pub(crate) gate: String,
    /// The phase's body lines, `####` promoted to `##`.
    pub(crate) body: Vec<String>,
}

/// A parsed plan document.
///
/// Why: the tracker body is assembled entirely from these fields, so a parse
/// that succeeded is a tracker that can be rendered without further reads.
/// What: the document's H1 title (which becomes the tracker's outcome), the
/// `## Epic plan` summary paragraph, the three required subsections as raw
/// markdown lines, one [`PhasePlan`] per phase, and the optional deferred
/// table.
/// Test: `plan_parses_the_committed_epic_plan`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EpicPlan {
    /// The document's H1 — the tracker's outcome, in plain words.
    pub(crate) outcome: String,
    /// The paragraph directly under `## Epic plan`.
    pub(crate) summary: Vec<String>,
    /// The `### Outcomes` list, verbatim.
    pub(crate) outcomes: Vec<String>,
    /// The `### Ratified decisions` table, verbatim.
    pub(crate) decisions: Vec<String>,
    /// The `### Ordering` prose, with the document's own phases block removed.
    pub(crate) ordering: Vec<String>,
    /// One entry per `### Phase:` heading, in document order.
    pub(crate) phases: Vec<PhasePlan>,
    /// The `### Deferred` table, or empty when the document declares none.
    pub(crate) deferred: Vec<String>,
}

/// Parse a plan document's `## Epic plan` section.
///
/// Why: the single entry point every epic-filing decision reads from, so the
/// schema is enforced in one place and `create` never sees a half-formed plan.
/// What: locates `## Epic plan`, splits its region on `### ` headings, and
/// validates each required subsection. `path` appears verbatim in every
/// refusal so the operator can see which document was read.
/// Test: see the module doc.
pub(crate) fn parse(path: &str, text: &str) -> Result<EpicPlan, PlanError> {
    let lines: Vec<&str> = text.lines().collect();
    let outcome = lines
        .iter()
        .find(|l| l.starts_with("# ") && !l.starts_with("##"))
        .map_or_else(String::new, |l| l[2..].trim().to_string());

    let start = lines
        .iter()
        .position(|l| l.trim_end() == EPIC_PLAN_HEADING)
        .ok_or_else(|| PlanError::MissingHeading {
            path: path.to_string(),
            heading: EPIC_PLAN_HEADING,
        })?;
    let end = lines[start + 1..]
        .iter()
        .position(|l| l.starts_with("## ") && !l.starts_with("###"))
        .map_or(lines.len(), |i| start + 1 + i);
    let region = &lines[start + 1..end];

    let sections = split_sections(region);
    let summary = trim_block(sections.leading);

    let mut outcomes = Vec::new();
    let mut decisions = Vec::new();
    let mut ordering: Option<Vec<String>> = None;
    let mut deferred = Vec::new();
    let mut phases = Vec::new();
    for (heading, body) in sections.subsections {
        match heading {
            h if h == OUTCOMES_HEADING => outcomes = trim_block(body),
            h if h == DECISIONS_HEADING => decisions = trim_block(body),
            h if h == ORDERING_HEADING => ordering = Some(trim_block(strip_phases_block(body))),
            h if h == DEFERRED_HEADING => deferred = trim_block(drop_marker_lines(body)),
            h if h.starts_with(PHASE_PREFIX) => {
                phases.push(parse_phase(path, h[PHASE_PREFIX.len()..].trim(), body)?);
            }
            _ => {}
        }
    }

    if outcomes.is_empty() {
        return Err(PlanError::MissingHeading {
            path: path.to_string(),
            heading: OUTCOMES_HEADING,
        });
    }
    if decisions.is_empty() {
        return Err(PlanError::MissingHeading {
            path: path.to_string(),
            heading: DECISIONS_HEADING,
        });
    }
    let ordering = ordering.ok_or_else(|| PlanError::MissingHeading {
        path: path.to_string(),
        heading: ORDERING_HEADING,
    })?;
    // #8447: an ordering section that exists and says nothing is the refusal
    // the pattern's own gate test asks for, not a defaulted empty string.
    if ordering.is_empty() {
        return Err(PlanError::EmptyOrdering {
            path: path.to_string(),
        });
    }
    if phases.is_empty() {
        return Err(PlanError::NoPhases {
            path: path.to_string(),
        });
    }

    Ok(EpicPlan {
        outcome,
        summary,
        outcomes,
        decisions,
        ordering,
        phases,
        deferred,
    })
}

/// A region split into its leading paragraph and its `### ` subsections.
struct Sections<'a> {
    /// Lines before the first `### ` heading.
    leading: Vec<&'a str>,
    /// `(heading, body)` for each `### ` heading, in document order.
    subsections: Vec<(&'a str, Vec<&'a str>)>,
}

/// Split a region on its level-3 headings.
///
/// Test: covered through `plan_parses_the_committed_epic_plan`.
fn split_sections<'a>(region: &[&'a str]) -> Sections<'a> {
    let mut leading = Vec::new();
    let mut subsections: Vec<(&'a str, Vec<&'a str>)> = Vec::new();
    for line in region.iter().copied() {
        if line.starts_with("### ") {
            subsections.push((line.trim_end(), Vec::new()));
        } else if let Some(last) = subsections.last_mut() {
            last.1.push(line);
        } else {
            leading.push(line);
        }
    }
    Sections {
        leading,
        subsections,
    }
}

/// Parse one `### Phase:` subsection.
///
/// Test: `plan_refuses_a_phase_with_no_gate_line`,
/// `plan_refuses_a_phase_with_no_acceptance_criteria`.
fn parse_phase(path: &str, title: &str, body: Vec<&str>) -> Result<PhasePlan, PlanError> {
    let gate = body
        .iter()
        .find(|l| l.trim_start().starts_with(GATE_PREFIX))
        .map(|l| l.trim().trim_start_matches(GATE_PREFIX).trim().to_string())
        .ok_or_else(|| PlanError::MissingGate {
            path: path.to_string(),
            phase: title.to_string(),
        })?;
    if !body.iter().any(|l| l.trim_end() == ACCEPTANCE_HEADING) {
        return Err(PlanError::MissingAcceptance {
            path: path.to_string(),
            phase: title.to_string(),
        });
    }
    let rest: Vec<&str> = body
        .into_iter()
        .filter(|l| !l.trim_start().starts_with(GATE_PREFIX))
        .collect();
    // `#### X` inside the plan becomes `## X` in the issue body, where it is a
    // top-level section rather than a nested one.
    let promoted = trim_block(rest)
        .into_iter()
        .map(|l| {
            l.strip_prefix("#### ")
                .map_or(l.clone(), |rest| format!("## {rest}"))
        })
        .collect();
    Ok(PhasePlan {
        title: title.to_string(),
        gate,
        body: promoted,
    })
}

/// Drop the document's own `phases` marker block from a section.
///
/// Why: D5 — issue numbers are never written back into the plan, so the block
/// the document carries is a `TBD` copy superseded by the tracker's. Copying it
/// into the tracker would put two phases blocks in one body, which is exactly
/// the shape `sync` refuses.
/// Test: `plan_strips_the_documents_own_phases_block`.
fn strip_phases_block(body: Vec<&str>) -> Vec<&str> {
    let mut out = Vec::new();
    let mut inside = false;
    for line in body {
        let trimmed = line.trim();
        if trimmed == PHASES_START {
            inside = true;
            continue;
        }
        if trimmed == PHASES_END {
            inside = false;
            continue;
        }
        if !inside {
            out.push(line);
        }
    }
    out
}

/// Drop any marker line, keeping the content between markers.
///
/// Why: a plan document's own `deferred` block already carries the marker
/// pair, and the tracker body wraps the same rows in markers of its own. Left
/// in, the tracker would carry two `deferred:start` lines — the shape
/// [`super::render::replace_block`] refuses.
/// Test: `plan_drops_the_documents_own_deferred_markers`.
fn drop_marker_lines(body: Vec<&str>) -> Vec<&str> {
    const MARKERS: [&str; 6] = [
        PHASES_START,
        PHASES_END,
        DEFERRED_START,
        DEFERRED_END,
        FOLLOWUPS_START,
        FOLLOWUPS_END,
    ];
    body.into_iter()
        .filter(|l| !MARKERS.contains(&l.trim()))
        .collect()
}

/// Drop leading and trailing blank lines, and own the rest.
fn trim_block(lines: Vec<&str>) -> Vec<String> {
    let first = lines.iter().position(|l| !l.trim().is_empty());
    let last = lines.iter().rposition(|l| !l.trim().is_empty());
    match (first, last) {
        (Some(a), Some(b)) => lines[a..=b].iter().map(|l| (*l).to_string()).collect(),
        _ => Vec::new(),
    }
}
