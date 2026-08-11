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
    StateColor, colorize, command_legend, format_session_row, picker_use_color,
    restart_confirm_hint, state_color, table_use_color,
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
        stale_assets_unchecked: false,
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
        PickerDecision::LaunchNew(None)
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

/// Why: the `tm ls` table writes to STDOUT via `println!`, so reusing the
/// picker's stderr gate would leak ANSI escapes into `tm ls | grep` whenever
/// stderr stayed a TTY. This pins that the table gate keys off its own stream.
#[test]
fn table_use_color_requires_stdout_tty() {
    assert!(!table_use_color(false));
    // With NO_COLOR unset (the default in this process) a TTY enables color.
    if std::env::var_os("NO_COLOR").is_none() {
        assert!(table_use_color(true));
    }
}

/// Why: the table gate must honour `NO_COLOR` identically to the picker gate —
/// any set value, including the empty string, disables color
/// (https://no-color.org). Shares the `no_color_env` serial group and the
/// save/restore convention with `picker_use_color_false_when_no_color_set_even_on_tty`.
#[test]
#[serial_test::serial(no_color_env)]
fn table_use_color_false_when_no_color_set_even_on_tty() {
    let prev = std::env::var_os("NO_COLOR");
    // SAFETY: serialized via #[serial(no_color_env)] against every other test
    // in this named group.
    unsafe {
        std::env::set_var("NO_COLOR", "");
    }
    let result = table_use_color(true);
    unsafe {
        match prev {
            Some(v) => std::env::set_var("NO_COLOR", v),
            None => std::env::remove_var("NO_COLOR"),
        }
    }
    assert!(
        !result,
        "an EMPTY NO_COLOR must still disable color on a stdout TTY"
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

// ── command_legend (#4965) ────────────────────────────────────────────────────

/// The no-sessions menu offers bare Enter and `n <name>`, and nothing to
/// delete or rename — there is no session to target.
#[test]
fn command_legend_empty_menu_shape() {
    let lines = command_legend(None);
    let keys: Vec<&str> = lines
        .iter()
        .map(|l| l.split("  ").next().unwrap())
        .collect();
    assert_eq!(keys, ["[Enter]", "[n <name>]", "[ls]", "[q]"]);
}

/// The populated menu leads with the launch slot and adds the delete/rename
/// rows.
#[test]
fn command_legend_populated_menu_shape() {
    let lines = command_legend(Some(4));
    let keys: Vec<&str> = lines
        .iter()
        .map(|l| l.split("  ").next().unwrap())
        .collect();
    assert_eq!(
        keys,
        [
            "[4]",
            "[n <name>]",
            "[d<N>]",
            "[r<N> <new-name>]",
            "[ls]",
            "[q]"
        ]
    );
}

/// #4965: `n <name>` must be self-documenting. `r1 <name>` right below it
/// takes a VERBATIM full session name while `n <name>` takes a leaf the daemon
/// wraps as `tm-<name>-NN` — adjacent rows with an unexplained `<name>` each
/// read as interchangeable.
#[test]
fn command_legend_n_row_names_the_resulting_session_shape() {
    let lines = command_legend(Some(4));
    let n_row = lines
        .iter()
        .find(|l| l.starts_with("[n <name>]"))
        .expect("the n row must be present");
    assert!(
        n_row.contains("tm-<name>-NN"),
        "the n row must spell out the name it produces: {n_row}"
    );
}

/// #4965 (LOW): BOTH menus are padded to the same column. The empty variant
/// was aligned and the populated one — the common case — was left ragged.
#[test]
fn command_legend_columns_are_aligned() {
    for lines in [command_legend(None), command_legend(Some(12))] {
        let offsets: Vec<usize> = lines
            .iter()
            .map(|l| {
                l.find("launch")
                    .or_else(|| l.find("delete"))
                    .or_else(|| l.find("rename"))
                    .or_else(|| l.find("re-print"))
                    .or_else(|| l.find("quit"))
                    .expect("every row has a description")
            })
            .collect();
        assert!(
            offsets.windows(2).all(|w| w[0] == w[1]),
            "descriptions must all start in the same column: {lines:#?}"
        );
    }
}

/// #4965: both #2148 restart prompts must name the key that actually declines.
///
/// Why: they used to read as a yes/no question while naming no refusal key at
/// all, so the operator had to invent one. `q` is the only key that leaves the
/// prompt without acting.
#[test]
fn restart_confirm_hint_names_the_refusal_key() {
    let hint = restart_confirm_hint(3);
    assert!(hint.contains("[3] confirms"), "confirm key missing: {hint}");
    assert!(hint.contains("[q] quits"), "refusal key missing: {hint}");
}

/// #4965: the hint must say what `n` does, because `n` spawns a session.
///
/// Why: this is the whole point of the wording fix. `n` is the universal "no"
/// AND the launch-new alias, so a prompt that reads as yes/no and stays silent
/// about `n` invites an operator to spawn a session while declining one. The
/// grammar is deliberately unchanged (`guided_picker_n_launches_new_unnamed`
/// still pins `n` -> `LaunchNew(None)`); only the prompt disowns the reading.
#[test]
fn restart_confirm_hint_disowns_n_as_no() {
    let hint = restart_confirm_hint(1);
    assert!(hint.contains("[n]"), "`n` unmentioned: {hint}");
    assert!(hint.contains("not \"no\""), "`n` not disowned: {hint}");
    assert!(
        hint.contains("NEW session"),
        "`n`'s real effect unstated: {hint}"
    );
}

// ── #5007: the degraded-store banner every listing surface prints ───────────

/// Why (#5007): on 2026-08-06 `tm ls` printed a normal-looking fleet while the
/// store was corrupt and every write was failing. The banner is what makes that
/// impossible — but a banner that fired on healthy listings would be noise on
/// every invocation, so the healthy case has to be silent.
/// What: a response with no `store_health` field produces no banner.
/// Test: this test.
#[test]
fn store_banner_is_absent_for_a_healthy_listing() {
    assert_eq!(
        super::store_degradation_banner(r#"{"sessions":[]}"#),
        None,
        "a healthy listing must print nothing"
    );
}

/// Why: a body this client cannot parse is not evidence that the STORE is
/// broken, and claiming so would send an operator to repair the wrong thing.
/// What: unparseable input produces no banner.
/// Test: this test.
#[test]
fn store_banner_is_absent_for_an_unparseable_body() {
    assert_eq!(super::store_degradation_banner("not json"), None);
}

/// Why: the corrupt case is the incident. The banner has to say the list is a
/// stale in-memory copy AND carry the daemon's message, which is what names the
/// file, the byte offset, and the repair command.
/// What: asserts both halves appear.
/// Test: this test.
#[test]
fn store_banner_names_corruption_and_the_repair_command() {
    let raw = r#"{"sessions":[],"store_health":{"message":"/x/sessions.json is corrupt: trailing characters (line 3755, column 2); a complete JSON document ends at byte 145090 of 146201, followed by 1111 trailing byte(s). Repair with `tm repair session-store`","corrupt":true,"observed_at":"2026-08-06T09:01:17Z"}}"#;
    let banner = super::store_degradation_banner(raw).expect("a corrupt store must warn");
    assert!(banner.contains("CORRUPT"), "{banner}");
    assert!(
        banner.contains("every write to the store is failing"),
        "{banner}"
    );
    assert!(banner.contains("byte 145090"), "{banner}");
    assert!(banner.contains("tm repair session-store"), "{banner}");
}

/// Why (#5027 review): the banner's STREAM is the whole contract. `tm ls
/// --json` writes the daemon's response body to stdout, so a banner on stdout
/// makes that JSON unparseable for every consumer. Nothing asserted it —
/// changing `eprintln!` to `println!` left the suite at `10 passed; 0 failed`
/// with the banner on stdout.
/// What: re-runs THIS test as a child process (the only way to observe the two
/// real streams — under `cargo test` libtest intercepts them) and asserts the
/// banner reaches stderr and never stdout.
/// Test: this test.
#[test]
fn store_banner_goes_to_stderr_so_json_stays_machine_readable() {
    const CHILD: &str = "TM_5027_BANNER_CHILD";
    const MARKER: &str = "CORRUPT";
    let raw = r#"{"sessions":[],"store_health":{"message":"/x/sessions.json is corrupt","corrupt":true,"observed_at":"2026-08-06T09:01:17Z"}}"#;

    if std::env::var_os(CHILD).is_some() {
        super::warn_if_store_degraded(raw);
        return;
    }

    // libtest names tests relative to the crate root, without the crate name.
    let module = module_path!();
    let module = module.split_once("::").map_or(module, |(_, rest)| rest);
    let name = format!("{module}::store_banner_goes_to_stderr_so_json_stays_machine_readable");
    let out = std::process::Command::new(std::env::current_exe().expect("test binary path"))
        .args([&name, "--exact", "--nocapture", "--test-threads", "1"])
        .env(CHILD, "1")
        .output()
        .expect("re-run this test as a child process");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stdout.contains("1 passed"),
        "the child must have actually run the case; stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stderr.contains(MARKER),
        "the banner must go to stderr; stderr: {stderr}"
    );
    assert!(
        !stdout.contains(MARKER),
        "the banner must NEVER reach stdout — `tm ls --json` writes the response body \
         there; stdout: {stdout}"
    );
}

/// Why: a transient read failure is genuinely different — the fallback may
/// already have cleared by the next listing — so the banner must say "stale",
/// not "corrupt", or it would send an operator to repair a healthy file.
/// What: asserts the non-corrupt wording.
/// Test: this test.
#[test]
fn store_banner_marks_a_transient_read_failure_as_stale_not_corrupt() {
    let raw = r#"{"sessions":[],"store_health":{"message":"session store I/O error: nfs hiccup","corrupt":false,"observed_at":"2026-08-06T09:01:17Z"}}"#;
    let banner = super::store_degradation_banner(raw).expect("a degraded store must warn");
    assert!(banner.contains("may be stale"), "{banner}");
    assert!(
        !banner.contains("CORRUPT"),
        "a transient failure must not be called corruption: {banner}"
    );
}
