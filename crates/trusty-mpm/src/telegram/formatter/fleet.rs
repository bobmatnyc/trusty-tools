//! Fleet-by-project formatter for the Telegram adapter.
//!
//! Why: extracted from `formatter/mod.rs` to keep that file under the 500-SLOC
//! production cap. The fleet formatter is a cohesive unit with its own glyph
//! helper and is large enough to warrant a sibling file.
//! What: [`format_fleet_by_project`] renders a [`ProjectFleetView`] slice as a
//! Telegram HTML message body grouped by project.
//! Test: `format_fleet_by_project_*` tests in `tests.rs`.

use teloxide::types::{InlineKeyboardButton, InlineKeyboardMarkup};

use crate::client::{ProjectFleetView, fleet_state_glyph};

use super::{callback_fits, html_escape, short_id};

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
/// Glyph mapping is delegated to [`crate::client::fleet_state_glyph`] so it
/// stays consistent with the Slack adapter.
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
                let dot = fleet_state_glyph(&s.state);
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

/// Build a `🎯 Focus` button row for every session in the fleet (TELUI-6, #1440).
///
/// Why: tapping a session in the `/fleet` list is the "session click" from the
/// TELUI-6 acceptance criteria — it focuses that session so plain messages route
/// to it. One button per session across all projects gives that click target on
/// a phone without typing an id.
/// What: emits one `focus:<id>` button per session (label prefixed with the
/// session name), skipping any id too long for Telegram's 64-byte callback-data
/// budget. Returns `None` when the fleet has no sessions (no keyboard warranted).
/// Test: `focus_keyboard_has_a_button_per_session` in `tests.rs`.
pub fn focus_keyboard(fleet: &[ProjectFleetView]) -> Option<InlineKeyboardMarkup> {
    let rows: Vec<Vec<InlineKeyboardButton>> = fleet
        .iter()
        .flat_map(|pf| pf.sessions.iter())
        .filter(|s| callback_fits(&s.id))
        .map(|s| {
            vec![InlineKeyboardButton::callback(
                format!("🎯 Focus — {}", s.name),
                format!("focus:{}", s.id),
            )]
        })
        .collect();
    if rows.is_empty() {
        None
    } else {
        Some(InlineKeyboardMarkup::new(rows))
    }
}

/// Build a single `🎯 Focus` button for one managed session's detail view.
///
/// Why: the `/get` detail card also offers a one-tap focus so the operator can
/// focus the session they just inspected without retyping its id.
/// What: returns a one-button keyboard with `focus:<id>` callback data, or `None`
/// when the id would overflow Telegram's callback-data budget.
/// Test: `session_focus_keyboard_builds_button` in `tests.rs`.
pub fn session_focus_keyboard(id: &str, name: &str) -> Option<InlineKeyboardMarkup> {
    if !callback_fits(id) {
        return None;
    }
    Some(InlineKeyboardMarkup::new(vec![vec![
        InlineKeyboardButton::callback(format!("🎯 Focus — {name}"), format!("focus:{id}")),
    ]]))
}
