//! Unit tests for the picker's #3723 (verbless color rows + duplicate-index
//! fix) and #3724 (rename input mode) changes.
//!
//! Why: kept in its own file (rather than growing `tests_behavior_c_tests.rs`
//! or `tests_behavior_d_tests.rs`, both already near the 1500-SLOC test cap)
//! and included via `session_picker.rs`'s `#[path]` `mod tests`, mirroring
//! the `rename_tests.rs`/`managed_tests.rs` convention used elsewhere in this
//! command tree.
//! What: fixture builders + `next_launch_slot_*`, `parse_picker_choice`'s new
//! `Rename` branch (`parse_picker_choice_rename_*`), and
//! `session_picker_render`'s row formatting/color (`format_session_row_*`,
//! `state_color_*`, `colorize_*`, `picker_use_color_*`).

use trusty_mpm::client::ManagedSessionSummary;

use super::{PickerDecision, next_launch_slot, parse_picker_choice};
use crate::commands::session_picker_render::{
    StateColor, colorize, format_session_row, picker_use_color, state_color,
};

/// Minimal `ManagedSessionSummary` fixture with a given `slot`.
fn session(name: &str, state: &str, slot: u32) -> ManagedSessionSummary {
    ManagedSessionSummary {
        id: format!("{name}-id"),
        name: name.to_string(),
        state: state.to_string(),
        persisted_state: None,
        workspace_path: None,
        repo_url: None,
        branch: None,
        created_at: None,
        last_activity_at: None,
        pending_decision: None,
        proposed_default: None,
        source_id: None,
        task: None,
        cwd: None,
        claude_session_id: None,
        deliverable_id: None,
        pane_id: None,
        injection_status: None,
        unresumable: false,
        stale_assets: false,
        attached: false,
        slot,
        deleted: false,
    }
}

// ── next_launch_slot (#3723 duplicate-index fix) ────────────────────────────

#[test]
fn next_launch_slot_matches_ascending_order() {
    let sessions = vec![session("s1", "active", 1), session("s2", "active", 2)];
    assert_eq!(next_launch_slot(&sessions), 3);
}

#[test]
fn next_launch_slot_survives_recency_reorder() {
    // Simulates the exact defect: `sort_sessions(Recent)` can put a
    // LOW-slot session last. The pre-fix `sessions.last().slot + 1`
    // computed 12 (colliding with the real session at slot 12); the fix
    // must use the true maximum across the whole slice regardless of order.
    let sessions = vec![
        session("s31", "active", 31),
        session("s12", "active", 12),
        session("s5", "active", 5), // least-recently-active, sorted last
    ];
    assert_eq!(
        next_launch_slot(&sessions),
        32,
        "must be (max slot)+1, not (last element's slot)+1"
    );
}

#[test]
fn next_launch_slot_survives_alpha_reorder() {
    // Alpha-sorted: "s12" < "s31" < "s5" lexicographically — slot 5 lands
    // last even though it is the LOWEST slot in the list.
    let sessions = vec![
        session("s12", "active", 12),
        session("s31", "active", 31),
        session("s5", "active", 5),
    ];
    assert_eq!(next_launch_slot(&sessions), 32);
}

#[test]
fn next_launch_slot_empty_sessions_is_one() {
    assert_eq!(next_launch_slot(&[]), 1);
}

#[test]
fn next_launch_slot_stale_slots_uses_positional_fallback() {
    // #3678 shape: every slot decodes to the `0` sentinel — positional
    // fallback (len + 1), unaffected by this fix (no real slots to collide
    // with).
    let sessions = vec![session("s1", "active", 0), session("s2", "active", 0)];
    assert_eq!(next_launch_slot(&sessions), 3);
}

#[test]
fn parse_picker_choice_launch_new_uses_max_slot_after_reorder() {
    // End-to-end through the public parse seam: typing the OLD (buggy)
    // "next slot" number must now resolve to the EXISTING session at that
    // slot, never to `LaunchNew` — proving the duplicate-`[N]` defect is
    // gone from the parse side too.
    let sessions = vec![
        session("s31", "active", 31),
        session("s12", "active", 12),
        session("s5", "active", 5),
    ];
    assert_eq!(
        parse_picker_choice("31", &sessions, false),
        PickerDecision::Resume(0),
        "31 must resolve to the real session at that slot, not collide with launch-new"
    );
    assert_eq!(
        parse_picker_choice("32", &sessions, false),
        PickerDecision::LaunchNew
    );
}

// ── PickerDecision::Rename parsing (#3724) ──────────────────────────────────

#[test]
fn parse_picker_choice_rename_no_space_between_prefix_and_number() {
    let sessions = vec![session("s1", "active", 1)];
    assert_eq!(
        parse_picker_choice("r1 new-name", &sessions, false),
        PickerDecision::Rename(0, "new-name".to_string())
    );
}

#[test]
fn parse_picker_choice_rename_space_between_prefix_and_number() {
    let sessions = vec![session("s1", "active", 1)];
    assert_eq!(
        parse_picker_choice("r 1 new-name", &sessions, false),
        PickerDecision::Rename(0, "new-name".to_string())
    );
}

#[test]
fn parse_picker_choice_rename_uppercase_prefix() {
    let sessions = vec![session("s1", "active", 1)];
    assert_eq!(
        parse_picker_choice("R1 new-name", &sessions, false),
        PickerDecision::Rename(0, "new-name".to_string())
    );
}

#[test]
fn parse_picker_choice_rename_trims_and_joins_multiword_name() {
    let sessions = vec![session("s1", "active", 1)];
    assert_eq!(
        parse_picker_choice("r1   my new name  \n", &sessions, false),
        PickerDecision::Rename(0, "my new name".to_string())
    );
}

#[test]
fn parse_picker_choice_rename_missing_name_is_unrecognised() {
    let sessions = vec![session("s1", "active", 1)];
    assert_eq!(
        parse_picker_choice("r1", &sessions, false),
        PickerDecision::Unrecognised
    );
}

#[test]
fn parse_picker_choice_rename_whitespace_only_name_is_unrecognised() {
    let sessions = vec![session("s1", "active", 1)];
    assert_eq!(
        parse_picker_choice("r1    ", &sessions, false),
        PickerDecision::Unrecognised
    );
}

#[test]
fn parse_picker_choice_rename_unknown_slot_is_unrecognised() {
    let sessions = vec![session("s1", "active", 1)];
    assert_eq!(
        parse_picker_choice("r99 new-name", &sessions, false),
        PickerDecision::Unrecognised
    );
}

#[test]
fn parse_picker_choice_rename_non_numeric_slot_is_unrecognised() {
    let sessions = vec![session("s1", "active", 1)];
    assert_eq!(
        parse_picker_choice("rabc new-name", &sessions, false),
        PickerDecision::Unrecognised
    );
}

#[test]
fn parse_picker_choice_rename_deleted_slot_is_slot_deleted() {
    let mut gone = session("gone", "deleted", 3);
    gone.deleted = true;
    let sessions = vec![gone];
    assert_eq!(
        parse_picker_choice("r3 new-name", &sessions, false),
        PickerDecision::SlotDeleted(0)
    );
}

// ── session_picker_render: colorize / state_color ───────────────────────────

#[test]
fn colorize_wraps_when_color_enabled() {
    assert_eq!(
        colorize("active", StateColor::Green, true),
        "\x1b[32mactive\x1b[0m"
    );
}

#[test]
fn colorize_plain_when_color_disabled() {
    assert_eq!(colorize("active", StateColor::Green, false), "active");
}

#[test]
fn colorize_plain_variant_never_wraps_even_with_color_enabled() {
    assert_eq!(
        colorize("weird-state", StateColor::Plain, true),
        "weird-state"
    );
}

#[test]
fn state_color_attached_wins_over_active() {
    assert_eq!(state_color("active", true), StateColor::AttachedCyan);
    assert_eq!(state_color("stopped", true), StateColor::AttachedCyan);
}

#[test]
fn state_color_maps_known_states() {
    assert_eq!(state_color("active", false), StateColor::Green);
    assert_eq!(state_color("stopped", false), StateColor::Dim);
    assert_eq!(state_color("errored", false), StateColor::Red);
    assert_eq!(state_color("provisioning", false), StateColor::Yellow);
}

#[test]
fn state_color_unknown_state_is_plain() {
    assert_eq!(state_color("some-future-state", false), StateColor::Plain);
}

#[test]
fn picker_use_color_requires_tty() {
    assert!(!picker_use_color(false));
}

/// Why: proves the NO_COLOR branch (previously untested — code-critic
/// finding on PR #3730), not just the TTY gate. Mutating a process-global
/// env var races any other test that reads/sets `NO_COLOR` concurrently
/// (the exact bug class fixed in commit 4716e4a5) — no other test in this
/// crate touches `NO_COLOR` today, but `#[serial_test::serial(no_color_env)]`
/// plus save/restore around the mutation keeps this test race-safe even if
/// one is added later, mirroring the `ENV_MUTEX`/save-restore convention in
/// `tests_behavior_c_tests.rs`.
#[test]
#[serial_test::serial(no_color_env)]
fn picker_use_color_false_when_no_color_set_even_on_tty() {
    let prev = std::env::var_os("NO_COLOR");
    // SAFETY: serialized via #[serial(no_color_env)] against every other
    // test in this named group; no other test in the crate touches
    // NO_COLOR, so this is the sole mutator of this key process-wide.
    unsafe {
        std::env::set_var("NO_COLOR", "1");
    }
    let result = picker_use_color(true);
    unsafe {
        match prev {
            Some(v) => std::env::set_var("NO_COLOR", v),
            None => std::env::remove_var("NO_COLOR"),
        }
    }
    assert!(
        !result,
        "NO_COLOR must disable color even when stderr is a TTY"
    );
}

// ── session_picker_render: format_session_row (#3723 verb removal) ─────────

#[test]
fn format_session_row_normal_is_verbless() {
    let s = session("tm-proj-01", "active", 5);
    let row = format_session_row(5, &s, false);
    assert_eq!(row, "[5] tm-proj-01 (active)");
    assert!(!row.contains("resume"));
    assert!(!row.contains("restart"));
}

#[test]
fn format_session_row_stopped_is_verbless() {
    let s = session("tm-proj-01", "stopped", 11);
    let row = format_session_row(11, &s, false);
    assert_eq!(row, "[11] tm-proj-01 (stopped)");
    assert!(!row.contains("restart"));
}

#[test]
fn format_session_row_attached_uses_attached_word() {
    let mut s = session("tm-proj-01", "active", 2);
    s.attached = true;
    let row = format_session_row(2, &s, false);
    assert_eq!(row, "[2] tm-proj-01 (attached)");
}

#[test]
fn format_session_row_deleted_notice() {
    let mut s = session("gone", "deleted", 7);
    s.deleted = true;
    let row = format_session_row(7, &s, false);
    assert_eq!(row, "[7] -- deleted --");
}

#[test]
fn format_session_row_unresumable_notice() {
    let mut s = session("tm-dead-01", "stopped", 9);
    s.unresumable = true;
    let row = format_session_row(9, &s, false);
    assert_eq!(
        row,
        "[9] tm-dead-01 (stopped) — DEAD: workspace removed; use [d9] to remove the record"
    );
}

#[test]
fn format_session_row_color_enabled_wraps_state_word_only() {
    let s = session("tm-proj-01", "active", 5);
    let row = format_session_row(5, &s, true);
    assert_eq!(row, "[5] tm-proj-01 (\x1b[32mactive\x1b[0m)");
}

#[test]
fn format_session_row_color_disabled_is_plain_text() {
    let s = session("tm-proj-01", "active", 5);
    let row = format_session_row(5, &s, false);
    assert!(
        !row.contains('\x1b'),
        "no ANSI escapes when use_color is false"
    );
}
