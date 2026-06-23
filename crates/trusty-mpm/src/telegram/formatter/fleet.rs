//! Fleet-by-project formatter for the Telegram adapter.
//!
//! Why: extracted from `formatter/mod.rs` to keep that file under the 500-SLOC
//! production cap. The fleet formatter is a cohesive unit with its own glyph
//! helper and is large enough to warrant a sibling file.
//! What: [`format_fleet_by_project`] renders a [`ProjectFleetView`] slice as a
//! Telegram HTML message body grouped by project.
//! Test: `format_fleet_by_project_*` tests in `tests.rs`.

use crate::client::ProjectFleetView;

use super::{html_escape, short_id};

/// Render managed sessions grouped by project as a Telegram HTML body.
///
/// Why: `/fleet` (WI-B, #1586) shows the operator a per-project breakdown so
/// related sessions are visible together; a flat list obscures which sessions
/// belong to which repo when working across many projects.
/// What: emits a heading, then one block per project — the project name as a
/// bold header followed by its session rows. Sessions with a pending decision
/// get a ⚠️ flag. Projects with no sessions show an `—` placeholder.
/// State glyphs follow the flat-list conventions:
///   🟢 active  🔴 stopped/errored/decommissioned  🟡 provisioning
/// Test: `format_fleet_by_project_renders_projects` in `tests.rs`.
pub fn format_fleet_by_project(fleet: &[ProjectFleetView]) -> String {
    if fleet.is_empty() {
        return "No registered projects.".to_string();
    }
    let mut text = String::from("<b>fleet by project</b>");
    for pf in fleet {
        text.push_str(&format!(
            "\n\n<b>{}</b> — <code>{}</code>",
            html_escape(&pf.project_name),
            html_escape(&pf.repo_url),
        ));
        if pf.sessions.is_empty() {
            text.push_str("\n  —");
        } else {
            for s in &pf.sessions {
                let dot = state_glyph(&s.state);
                let flag = if s.pending_decision.is_some() {
                    " ⚠️"
                } else {
                    ""
                };
                text.push_str(&format!(
                    "\n  {dot} <code>{}</code> {} [{}]{flag}",
                    short_id(&s.id),
                    html_escape(&s.name),
                    html_escape(&s.state),
                ));
            }
        }
    }
    text
}

/// Choose a status glyph for a managed session lifecycle state word.
///
/// Why: `format_fleet_by_project` and any future multi-project formatter need
/// the same glyph conventions as the flat-list formatter but expressed as a
/// shared helper rather than duplicated inline.
/// What: `active` → 🟢, `provisioning` → 🟡, anything else → 🔴.
/// Test: covered by `format_fleet_by_project_renders_projects` in `tests.rs`.
fn state_glyph(state: &str) -> &'static str {
    match state.to_ascii_lowercase().as_str() {
        "active" => "🟢",
        "provisioning" => "🟡",
        _ => "🔴",
    }
}
