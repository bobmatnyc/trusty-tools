//! Per-section status of the composed PM prompt (#8533).
//!
//! Why: `tm sessions instructions` logged "applying ... section=Identity" while
//! the package `## Identity` still opened the prompt, so the report and the
//! outcome disagreed. The operator needs one table that says, per section,
//! whether the package text, the project's text, or a decline is in force —
//! and that table must be checked against the prompt actually composed.
//! What: [`section_statuses`] derives each section's state from the same scan
//! and the same [`InstructionPackage::with_overrides`] call the composer runs;
//! [`render_section_report`] prints it and flags any overridden section whose
//! text is not in the composed prompt.
//! Test: `instruction_section_report_tests.rs`.

use std::path::{Path, PathBuf};

use crate::core::claude_md_sections::{Rejection, scan_project, section_token};
use crate::core::instruction_package::{InstructionPackage, SectionId};

/// What is in force for one section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SectionState {
    /// The bundled package text.
    Package,
    /// The project's override text replaces the package text.
    Overridden,
    /// The project declared an override and it was not applied; the reason.
    Declined(String),
}

/// One row of the section report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionStatus {
    /// The section.
    pub section: SectionId,
    /// Its `CLAUDE.md` marker token.
    pub token: &'static str,
    /// What is in force.
    pub state: SectionState,
    /// Where the project's marker block sits, when it declared one.
    pub source: Option<(PathBuf, usize)>,
    /// First content line of the override body, used to check the prompt.
    pub probe: Option<String>,
    /// Whether a pinned framework-feature block stays in force (#8533).
    pub pinned_kept: bool,
}

/// The section a rejection names; `None` for a whole-package revert.
fn rejected_section(rejection: &Rejection) -> Option<SectionId> {
    match rejection {
        Rejection::NotOverridable { section, .. }
        | Rejection::UnknownSection { section }
        | Rejection::NoTextBlock { section }
        | Rejection::EmptyBody { section }
        | Rejection::WouldEmitNothing { section }
        | Rejection::Invalidates { section, .. } => Some(*section),
        Rejection::PackageInvalid(_) => None,
    }
}

/// First line of `body` that is neither blank nor an HTML comment line.
fn probe_line(body: &str) -> Option<String> {
    body.lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with("<!--"))
        .map(str::to_string)
}

/// Per-section status of the prompt this project composes.
///
/// Why: see the module docs. Takes `roster_present` because the roster alone
/// selects the composer, and the roster-absent string assembly can place only
/// `WORKFLOW`, `MEMORY` and `AGENT-DELEGATION`.
/// What: one row per [`SectionId::CANONICAL`] entry. A section with no marker
/// block is [`SectionState::Package`]; one whose override the composer applied
/// is [`SectionState::Overridden`]; one whose override it declined carries the
/// reason.
/// Test: `identity_and_a_former_core_section_report_overridden`,
/// `a_core_override_reports_declined`, `the_roster_absent_path_reports_what_it_cannot_place`.
pub fn section_statuses(
    package: &InstructionPackage,
    project_dir: &Path,
    roster_present: bool,
) -> Vec<SectionStatus> {
    let scanned = scan_project(project_dir);
    let mut declined: Vec<(SectionId, String)> = Vec::new();
    if roster_present {
        let (_, rejected) = package.with_overrides(&scanned.overrides);
        for rejection in &rejected {
            match rejected_section(rejection) {
                Some(section) => declined.push((section, rejection.to_string())),
                None => declined.extend(
                    scanned
                        .overrides
                        .iter()
                        .map(|o| (o.section, rejection.to_string())),
                ),
            }
        }
    } else {
        declined.extend(
            scanned
                .overrides
                .iter()
                .filter(|o| {
                    !matches!(
                        o.section,
                        SectionId::Workflow | SectionId::Memory | SectionId::AgentDelegation
                    )
                })
                .map(|o| {
                    (
                        o.section,
                        "no agent is deployed, so the roster-absent composer cannot place \
                         this section"
                            .to_string(),
                    )
                }),
        );
    }

    SectionId::CANONICAL
        .into_iter()
        .map(|section| {
            let over = scanned.overrides.iter().find(|o| o.section == section);
            let state = match (over, declined.iter().find(|(s, _)| *s == section)) {
                (None, _) => SectionState::Package,
                (Some(_), Some((_, reason))) => SectionState::Declined(reason.clone()),
                (Some(_), None) => SectionState::Overridden,
            };
            SectionStatus {
                section,
                token: section_token(section),
                pinned_kept: state == SectionState::Overridden
                    && package
                        .blocks
                        .iter()
                        .any(|b| b.section == section && b.pinned),
                state,
                source: over.map(|o| (o.host.clone(), o.line)),
                probe: over.and_then(|o| probe_line(&o.body)),
            }
        })
        .collect()
}

/// Render the report, checked against the composed `prompt`.
///
/// Why: a report computed beside the composition could still drift from it;
/// checking each override's text against the delivered prompt is what makes a
/// disagreement visible instead of silent (#8533).
/// What: one line per section — token, state, marker location — plus
/// `framework feature kept` for an overridden section with a pinned block, and
/// `NOT FOUND in the composed prompt` when an overridden section's first content
/// line is absent from `prompt`.
/// Test: `render_flags_an_override_missing_from_the_prompt`.
pub fn render_section_report(statuses: &[SectionStatus], prompt: &str) -> String {
    let mut out = String::from("instruction sections (package / overridden / declined):\n");
    for row in statuses {
        let location = row
            .source
            .as_ref()
            .map(|(host, line)| format!("  {}:{line}", host.display()))
            .unwrap_or_default();
        let detail = match &row.state {
            SectionState::Package => "package".to_string(),
            SectionState::Overridden => {
                let mut s = "overridden".to_string();
                if row.pinned_kept {
                    s.push_str(" (framework feature kept)");
                }
                if row.probe.as_deref().is_some_and(|p| !prompt.contains(p)) {
                    s.push_str(" (override text NOT FOUND in the composed prompt)");
                }
                s
            }
            SectionState::Declined(reason) => format!("declined: {reason}"),
        };
        out.push_str(&format!("  {:<34} {detail}{location}\n", row.token));
    }
    out
}

/// The rendered section report for `project_dir`, checked against `prompt`.
///
/// Why: the one call `tm sessions instructions` makes, so the CLI cannot pick
/// a different package or roster test than the composer.
/// What: the bundled package, the live roster's presence, then
/// [`section_statuses`] and [`render_section_report`].
/// Test: `instructions_reports_section_status_and_project_style`.
pub fn section_report_for(project_dir: &Path, prompt: &str) -> String {
    let roster_present =
        crate::core::delegation_authority::deployed_roster_section(project_dir).is_some();
    match crate::core::bundled_pm_package::bundled_fallback_package() {
        Ok(package) => render_section_report(
            &section_statuses(package, project_dir, roster_present),
            prompt,
        ),
        Err(err) => format!("instruction sections: the bundled manifest is unusable: {err}\n"),
    }
}

#[cfg(test)]
#[path = "instruction_section_report_tests.rs"]
mod tests;
