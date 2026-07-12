//! Read-only Deliverable/Milestone overlay — DOC-35 §10.8 `show`, #2383.
//!
//! Why: §10.8's CLI sketch has a `deliverables show <project> <id>` /
//! `milestones show <project> <id>` surface with "includes bound sessions,
//! read-only". Issue #2383's own scope line asks for something slightly
//! broader — a project-scoped LIST of Deliverables and Milestones with
//! status, reachable from the Projects pane — which is what this overlay
//! renders. **Disclosed deviation from a literal §10.8 `show` reading**: this
//! view does not drill into one Deliverable's bound-sessions detail; it lists
//! every Deliverable/Milestone for the selected project with its status in
//! one screen, matching the issue's own "Deliverable/Milestone view... lists
//! the selected project's deliverables and milestones with status" framing
//! more directly than a single-record `show` would. Per-Deliverable
//! bound-session drill-down is left for a follow-up if wanted — out of this
//! slice's scope. It is a strict ADDITION to the existing 4-pane layout
//! (§5.1): a centred overlay on top of the frame, never replacing a pane,
//! mirroring the `tui::dashboard` help-overlay's `Clear` + centred-`Rect`
//! pattern (`tui/dashboard/mod.rs::render_help_overlay`).
//! What: [`centered_rect`] (a local copy of the dashboard helper — this
//! module intentionally does not depend on `tui::dashboard`, keeping
//! `project_ctl` self-contained per its existing module boundary),
//! [`body_lines`] (the pure Deliverable/Milestone → text projection, unit
//! tested without a terminal), and [`render`] (the terminal-touching overlay
//! draw, called from [`super::super::layout::render`] only when
//! [`DeliverableView`] is `Some`).
//! Test: `tests` covers `body_lines`' status/tier/date formatting and the
//! empty-list placeholder text.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

use crate::deliverable::{Deliverable, EstimationTier, Milestone, MilestoneStatus};
use crate::tui::project_ctl::state::DeliverableView;

/// Compute a centred sub-rectangle for the overlay, floored to `area`'s size.
///
/// Why: a local copy of `tui::dashboard::centered_rect` — `project_ctl` does
/// not depend on the `dashboard` module for anything else, and this is a
/// three-line pure function, not worth a cross-module dependency to share.
/// What: same behavior as the dashboard original: centers a `width x height`
/// box within `area`, clamped so it never exceeds `area`'s own bounds.
fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    }
}

/// The uppercase tier label for an [`EstimationTier`] (DOC-30 Decision #2).
fn tier_str(tier: EstimationTier) -> &'static str {
    match tier {
        EstimationTier::S => "S",
        EstimationTier::M => "M",
        EstimationTier::L => "L",
        EstimationTier::Xl => "XL",
    }
}

/// The kebab-case wire label for a [`MilestoneStatus`] (§10.5).
///
/// Why: unlike [`DeliverableStatus::as_str`], `MilestoneStatus` has no
/// existing `as_str` — this mirrors its `#[serde(rename_all = "kebab-case")]`
/// encoding without adding a serde round trip just to get display text.
fn milestone_status_str(status: MilestoneStatus) -> &'static str {
    match status {
        MilestoneStatus::Proposed => "proposed",
        MilestoneStatus::InProgress => "in-progress",
        MilestoneStatus::Complete => "complete",
        MilestoneStatus::Shipped => "shipped",
    }
}

/// One Deliverable's display line: `  [<status>] <name>  (<tier>)`.
fn deliverable_line(d: &Deliverable) -> String {
    format!(
        "  [{:<11}] {}  ({})",
        d.status.as_str(),
        d.name,
        tier_str(d.estimated_effort)
    )
}

/// One Milestone's display line: `  [<status>] <name>  target: <YYYY-MM-DD>`.
fn milestone_line(m: &Milestone) -> String {
    format!(
        "  [{:<11}] {}  target: {}",
        milestone_status_str(m.status),
        m.name,
        m.target_date.format("%Y-%m-%d")
    )
}

/// Build the overlay's full body text for one [`DeliverableView`].
///
/// Why: a pure builder keeps the exact section headers / placeholder text /
/// per-row formatting unit-testable without a terminal.
/// What: `DELIVERABLES (N)` then one [`deliverable_line`] per entry (or a
/// `(none)` placeholder), a blank separator, then the same shape for
/// `MILESTONES (N)` / [`milestone_line`].
/// Test: `body_lines_lists_deliverables_and_milestones_with_status`,
/// `body_lines_shows_placeholder_when_empty`.
pub fn body_lines(view: &DeliverableView) -> Vec<String> {
    let mut lines = vec![format!("DELIVERABLES ({})", view.deliverables.len())];
    if view.deliverables.is_empty() {
        lines.push("  (none)".to_string());
    } else {
        lines.extend(view.deliverables.iter().map(deliverable_line));
    }
    lines.push(String::new());
    lines.push(format!("MILESTONES ({})", view.milestones.len()));
    if view.milestones.is_empty() {
        lines.push("  (none)".to_string());
    } else {
        lines.extend(view.milestones.iter().map(milestone_line));
    }
    lines
}

/// Width/height of the overlay box, in terminal cells.
const OVERLAY_WIDTH: u16 = 72;
const OVERLAY_HEIGHT: u16 = 20;

/// Render the Deliverable/Milestone overlay, when open.
///
/// Why: the single entry point [`super::super::layout::render`] calls AFTER
/// the base 4-pane frame, only when [`super::super::state::ProjectCtlState::deliverable_view`]
/// is `Some` — mirrors the dashboard help-overlay's "draw the base frame,
/// then float the overlay on top via `Clear`" sequencing.
/// What: a centred, bordered, titled (`Deliverables/Milestones — <project>`)
/// `Paragraph` over [`body_lines`], scrolled by `view.scroll`.
pub fn render(frame: &mut Frame, view: &DeliverableView) {
    let area = centered_rect(OVERLAY_WIDTH, OVERLAY_HEIGHT, frame.area());
    frame.render_widget(Clear, area);
    let title = Line::from(vec![Span::styled(
        format!("Deliverables/Milestones — {}", view.project_name),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )]);
    let body = body_lines(view).join("\n");
    let footer = "\n[↑↓] scroll  [Esc/v] close";
    frame.render_widget(
        Paragraph::new(format!("{body}{footer}"))
            .style(Style::default().fg(Color::Reset))
            .scroll((view.scroll, 0))
            .block(Block::default().borders(Borders::ALL).title(title)),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deliverable::{DeliverableId, DeliverableKind, DeliverableStatus, MilestoneId};
    use chrono::{DateTime, Utc};

    fn deliverable(name: &str, status: DeliverableStatus) -> Deliverable {
        Deliverable {
            id: DeliverableId::new(),
            project_name: "widget".to_string(),
            name: name.to_string(),
            description: String::new(),
            kind: DeliverableKind::Feature,
            ticket_ref: None,
            spec_ref: None,
            status,
            estimated_effort: EstimationTier::M,
            created_at: Utc::now(),
            target_date: None,
        }
    }

    fn milestone(name: &str, status: MilestoneStatus) -> Milestone {
        Milestone {
            id: MilestoneId::new(),
            project_name: "widget".to_string(),
            name: name.to_string(),
            description: String::new(),
            target_date: DateTime::parse_from_rfc3339("2026-09-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            status,
            deliverables: vec![],
            created_at: Utc::now(),
        }
    }

    #[test]
    fn body_lines_lists_deliverables_and_milestones_with_status() {
        let view = DeliverableView {
            project_name: "widget".to_string(),
            deliverables: vec![deliverable("OAuth2 flow", DeliverableStatus::InProgress)],
            milestones: vec![milestone("v1.0 Alpha", MilestoneStatus::Proposed)],
            scroll: 0,
        };
        let lines = body_lines(&view).join("\n");
        assert!(lines.contains("DELIVERABLES (1)"));
        assert!(lines.contains("OAuth2 flow"));
        assert!(lines.contains("in-progress"));
        assert!(lines.contains("(M)"));
        assert!(lines.contains("MILESTONES (1)"));
        assert!(lines.contains("v1.0 Alpha"));
        assert!(lines.contains("proposed"));
        assert!(lines.contains("2026-09-01"));
    }

    #[test]
    fn body_lines_shows_placeholder_when_empty() {
        let view = DeliverableView {
            project_name: "widget".to_string(),
            deliverables: vec![],
            milestones: vec![],
            scroll: 0,
        };
        let lines = body_lines(&view).join("\n");
        assert!(lines.contains("DELIVERABLES (0)"));
        assert!(lines.contains("MILESTONES (0)"));
        assert_eq!(lines.matches("(none)").count(), 2);
    }

    #[test]
    fn tier_str_maps_every_tier() {
        assert_eq!(tier_str(EstimationTier::S), "S");
        assert_eq!(tier_str(EstimationTier::Xl), "XL");
    }

    #[test]
    fn milestone_status_str_matches_wire_format() {
        assert_eq!(
            milestone_status_str(MilestoneStatus::InProgress),
            "in-progress"
        );
        assert_eq!(milestone_status_str(MilestoneStatus::Shipped), "shipped");
    }

    #[test]
    fn centered_rect_clamps_to_area_bounds() {
        let small = Rect {
            x: 0,
            y: 0,
            width: 10,
            height: 5,
        };
        let r = centered_rect(OVERLAY_WIDTH, OVERLAY_HEIGHT, small);
        assert_eq!(r.width, 10);
        assert_eq!(r.height, 5);
    }
}
