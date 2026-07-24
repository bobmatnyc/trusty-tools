//! Config-edit form overlay — DOC-35 §6 configurator, #2120.
//!
//! Why: the TUI half of the deterministic configurator (§6 RESOLVED: CLI +
//! TUI form, same epic). Mirrors `deliverables_view`'s "centred overlay on
//! top of the frame, never replacing a pane" pattern, but editable rather
//! than read-only: each of the five rows shows its label, current buffer,
//! and (when the buffer differs from what loaded) a `*` changed-marker, plus
//! an inline error line when the last submit was rejected.
//! What: [`body_lines`] (the pure field-list → text projection, unit tested
//! without a terminal) and [`render`] (the terminal-touching overlay draw,
//! called from [`super::super::layout::render`] only when
//! [`ConfigFormView`] is `Some`).
//! Test: `tests` covers `body_lines`' field rendering, the changed-marker,
//! and the inline-error line.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

use crate::tui::project_ctl::state::ConfigFormView;

/// Compute a centred sub-rectangle for the overlay, floored to `area`'s size.
///
/// Why: a local copy of `tui::dashboard::centered_rect` / the identical
/// helper in `deliverables_view.rs` — each overlay module keeps its own
/// three-line copy rather than sharing across modules (see that module's doc
/// for the rationale, which applies unchanged here).
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

/// One field row: `label: value` with a trailing `*` when the buffer differs
/// from the loaded original (blank-vs-`(unset)` normalized for the compare
/// so a genuinely untouched unset field never shows a spurious marker).
fn field_line(label: &str, original: Option<&str>, value: &str, focused: bool) -> String {
    let marker = if value.trim() != original.unwrap_or("") {
        "*"
    } else {
        ""
    };
    let cursor = if focused { "> " } else { "  " };
    let shown = if value.is_empty() { "(empty)" } else { value };
    format!("{cursor}{label}: {shown}{marker}")
}

/// Build the overlay's full body text for one [`ConfigFormView`].
///
/// Why: a pure builder keeps the exact row order / changed-marker / cursor /
/// error-line formatting unit-testable without a terminal.
/// What: one line per field (`default_branch`/`description`/`stack_hint`/
/// `gh_user`/`tags`, in tab order), a `>` cursor on the focused row, a `*`
/// changed-marker, and — when `error` is `Some` — a trailing blank line plus
/// `ERROR: <msg>`.
/// Test: `body_lines_marks_the_focused_row`, `body_lines_marks_changed_fields`,
/// `body_lines_shows_inline_error`.
pub fn body_lines(view: &ConfigFormView) -> Vec<String> {
    use crate::tui::project_ctl::state::ConfigFormFocus;

    let mut lines = vec![
        field_line(
            "default_branch",
            view.default_branch.original.as_deref(),
            &view.default_branch.value,
            view.focus == ConfigFormFocus::DefaultBranch,
        ),
        field_line(
            "description",
            view.description.original.as_deref(),
            &view.description.value,
            view.focus == ConfigFormFocus::Description,
        ),
        field_line(
            "stack_hint",
            view.stack_hint.original.as_deref(),
            &view.stack_hint.value,
            view.focus == ConfigFormFocus::StackHint,
        ),
        field_line(
            "gh_user",
            view.gh_user.original.as_deref(),
            &view.gh_user.value,
            view.focus == ConfigFormFocus::GhUser,
        ),
        field_line(
            "tags",
            Some(&view.tags.original.join(", ")),
            &view.tags.value,
            view.focus == ConfigFormFocus::Tags,
        ),
    ];
    if let Some(err) = &view.error {
        lines.push(String::new());
        lines.push(format!("ERROR: {err}"));
    }
    lines
}

/// Width/height of the overlay box, in terminal cells.
const OVERLAY_WIDTH: u16 = 72;
const OVERLAY_HEIGHT: u16 = 12;

/// Render the config form overlay, when open.
///
/// Why: the single entry point [`super::super::layout::render`] calls AFTER
/// the base 4-pane frame, only when
/// [`super::super::state::ProjectCtlState::config_form`] is `Some` — mirrors
/// `deliverables_view::render`'s sequencing.
/// What: a centred, bordered, titled (`Config — <project>`) `Paragraph` over
/// [`body_lines`] plus a footer key-hint line.
pub fn render(frame: &mut Frame, view: &ConfigFormView) {
    let area = centered_rect(OVERLAY_WIDTH, OVERLAY_HEIGHT, frame.area());
    frame.render_widget(Clear, area);
    let title = Line::from(vec![Span::styled(
        format!("Config — {}", view.project_name),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )]);
    let body = body_lines(view).join("\n");
    let footer = "\n[Tab] next field  [Enter] submit  [Esc] cancel";
    frame.render_widget(
        Paragraph::new(format!("{body}{footer}"))
            .style(Style::default().fg(Color::Reset))
            .block(Block::default().borders(Borders::ALL).title(title)),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::Project;
    use crate::tui::project_ctl::state::ConfigFormFocus;

    fn project() -> Project {
        Project {
            name: "widget".to_string(),
            repo_url: "https://github.com/acme/widget".to_string(),
            default_branch: "main".to_string(),
            stack_hint: Some("rust".to_string()),
            tags: vec!["backend".to_string()],
            description: Some("the widget".to_string()),
            gh_user: Some("acme-bot".to_string()),
            gh_account: None,
            github: None,
            commit_name: None,
            commit_email: None,
            worktree: None,
        }
    }

    /// Build a form the same way `ProjectCtlState::open_config_form` would.
    fn form() -> ConfigFormView {
        let mut state = crate::tui::project_ctl::state::ProjectCtlState {
            projects_full: [("widget".to_string(), project())].into_iter().collect(),
            ..Default::default()
        };
        state.open_config_form("widget");
        state.config_form.expect("form should be open")
    }

    #[test]
    fn body_lines_marks_the_focused_row() {
        let lines = body_lines(&form()).join("\n");
        assert!(lines.contains("> default_branch: main"));
        assert!(lines.contains("  description: the widget"));
    }

    #[test]
    fn body_lines_marks_changed_fields() {
        let mut view = form();
        view.stack_hint.value = "python".to_string();
        let lines = body_lines(&view).join("\n");
        assert!(lines.contains("stack_hint: python*"));
        // Untouched fields carry no marker.
        assert!(lines.contains("gh_user: acme-bot") && !lines.contains("gh_user: acme-bot*"));
    }

    #[test]
    fn body_lines_shows_inline_error() {
        let mut view = form();
        view.error = Some("project name is the identity key".to_string());
        let lines = body_lines(&view);
        assert!(
            lines
                .iter()
                .any(|l| l.contains("ERROR: project name is the identity key"))
        );
    }

    #[test]
    fn body_lines_omits_error_section_when_none() {
        let lines = body_lines(&form());
        assert!(!lines.iter().any(|l| l.starts_with("ERROR:")));
    }

    #[test]
    fn field_line_shows_empty_placeholder_for_a_blank_buffer() {
        let line = field_line("description", Some("old"), "", false);
        assert!(line.contains("(empty)"));
        assert!(
            line.contains('*'),
            "cleared from a non-empty original is a change"
        );
    }

    #[test]
    fn tags_row_uses_the_joined_original_as_its_comparison_baseline() {
        let view = form();
        assert_eq!(view.focus, ConfigFormFocus::DefaultBranch);
        let lines = body_lines(&view).join("\n");
        // Unedited tags buffer must not show a changed-marker.
        assert!(lines.contains("tags: backend") && !lines.contains("tags: backend*"));
    }
}
