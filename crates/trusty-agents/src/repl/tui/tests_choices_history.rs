//! Tests for the #3346 fix: disambiguating a first Up/Down keypress over
//! `app.choices` between picker navigation and shell-style history recall.
//! Split out of `tests_input.rs` (#357 SLOC cap) so both files stay under
//! the 500-SLOC production cap. Flat re-exports in `mod.rs` make every item
//! reachable via `super::*`.

#![cfg(test)]

use super::*;

/// Backdate `app.choices_opened_at` so `should_navigate_picker` treats the
/// picker as stale without an actual `sleep` in the test (see
/// `PICKER_STALE_AFTER` in `keys.rs`).
fn backdate_choices_opened_at(a: &mut ReplApp) {
    a.choices_opened_at = std::time::Instant::now()
        .checked_sub(std::time::Duration::from_secs(10))
        .or(a.choices_opened_at);
}

/// Why (#3346): The #3325 fix dismissed a stale picker on any key EXCEPT
/// Up/Down/Enter/Esc, leaving Up/Down as first keystroke over a stale
/// `detect_choices` list still captured as picker navigation instead of the
/// shell-style history recall the user almost certainly wanted (empty input
/// buffer = nothing typed toward a picker interaction yet, and the picker
/// has been sitting well past `PICKER_STALE_AFTER`).
/// What: Populate `app.choices` the way `push_assistant` -> `detect_choices`
/// does, backdate `choices_opened_at` past the staleness window, leave the
/// input buffer empty, and send a single `Up`. Assert the stale picker is
/// dismissed AND the normal Up-arrow history-recall path ran (`last_prompt`
/// copied into `input_buf`), exactly like `repl_app_up_arrow_recalls_last_prompt`
/// with no picker in the way.
/// Test: `repl_app_first_up_over_stale_picker_recalls_history` (this test).
#[test]
fn repl_app_first_up_over_stale_picker_recalls_history() {
    let mut a = ReplApp::new("ctrl".into(), "u".into());
    a.push_assistant("Pick one:\n1. Apple\n2. Banana\n3. Orange", false);
    assert!(!a.choices.is_empty(), "setup: picker must be showing");
    assert!(a.input_buf.is_empty(), "setup: buffer must be empty");
    backdate_choices_opened_at(&mut a);
    a.last_prompt = "earlier prompt".to_string();

    let r = handle_key(&mut a, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));

    assert_eq!(r, None);
    assert!(
        a.choices.is_empty(),
        "first Up over a stale picker with an empty buffer must dismiss it"
    );
    assert_eq!(
        a.input_buf, "earlier prompt",
        "the Up keypress must fall through to normal history recall"
    );
}

/// Why (#3346): Mirrors `repl_app_first_up_over_stale_picker_recalls_history`
/// for `Down`, and for the tagged `/switch` picker (not just the untagged
/// LLM list) — #3325's own background note calls out both kinds as subject
/// to the same staleness class.
/// What: Populate `app.choices` the way the `/switch` handler does
/// (`choices_context = Some("switch")`), backdate `choices_opened_at` past
/// the staleness window, leave the buffer empty, seed `history` +
/// `history_idx` so `history_next` has somewhere to go, and send a single
/// `Down`. Assert the picker (including its context tag) is dismissed AND
/// `history_next` ran.
/// Test: `repl_app_first_down_over_stale_picker_recalls_history` (this test).
#[test]
fn repl_app_first_down_over_stale_picker_recalls_history() {
    let mut a = ReplApp::new("ctrl".into(), "u".into());
    a.choices = vec!["ctrl".to_string(), "Izzie".to_string()];
    a.choice_cursor = 0;
    a.choices_context = Some("switch".to_string());
    backdate_choices_opened_at(&mut a);
    a.history = vec!["cmd one".to_string(), "cmd two".to_string()];
    a.history_idx = Some(0);
    assert!(a.input_buf.is_empty(), "setup: buffer must be empty");

    let r = handle_key(&mut a, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));

    assert_eq!(r, None);
    assert!(
        a.choices.is_empty(),
        "first Down over a stale /switch picker with an empty buffer must \
         dismiss it"
    );
    assert!(
        a.choices_context.is_none(),
        "the switch context tag must be cleared along with the choices"
    );
    assert_eq!(
        a.input_buf, "cmd two",
        "the Down keypress must fall through to normal history_next"
    );
}

/// Why (#3346): A picker that was *just* populated — no timestamp old
/// enough to cross `PICKER_STALE_AFTER` — must still navigate on its first
/// Up/Down even with an empty buffer. This is the normal-use case
/// `PICKER_STALE_AFTER`'s doc calls out (mirrors
/// `inline_choices_switch_context_dispatches_submit` in `tests_render.rs`,
/// which pins the same expectation for a hand-built `/switch` picker with no
/// timestamp at all).
/// What: Populate an untagged LLM-offered list via `push_assistant` (which
/// stamps `choices_opened_at = Instant::now()`), leave the buffer empty, and
/// send a `Down` immediately. Assert `choice_cursor` moved and the picker
/// survived.
/// Test: `repl_app_fresh_picker_navigates_on_first_press` (this test).
#[test]
fn repl_app_fresh_picker_navigates_on_first_press() {
    let mut a = ReplApp::new("ctrl".into(), "u".into());
    a.push_assistant("Pick one:\n1. Apple\n2. Banana\n3. Orange", false);
    assert!(a.input_buf.is_empty(), "setup: buffer must be empty");

    let r = handle_key(&mut a, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));

    assert_eq!(r, None);
    assert!(
        !a.choices.is_empty(),
        "a freshly-populated picker must not be dismissed by its first Down"
    );
    assert_eq!(
        a.choice_cursor, 1,
        "Down must navigate a freshly-populated picker on the first press"
    );
}

/// Why (#3346): The disambiguation in `should_navigate_picker` must not
/// regress normal picker use — once the user has genuinely navigated a
/// stale (non-live-slash) picker, further Up/Down must keep navigating even
/// though the input buffer stays empty and the picker has crossed
/// `PICKER_STALE_AFTER`.
/// What: Populate an untagged LLM-offered list, backdate `choices_opened_at`
/// past the staleness window, mark it as already-navigated
/// (`choices_navigated = true`, simulating a prior Down/Up press), and send
/// a `Down`. Assert `choice_cursor` moved and the picker survived instead of
/// being reinterpreted as history recall.
/// Test: `repl_app_stale_picker_navigates_after_first_press` (this test).
#[test]
fn repl_app_stale_picker_navigates_after_first_press() {
    let mut a = ReplApp::new("ctrl".into(), "u".into());
    a.push_assistant("Pick one:\n1. Apple\n2. Banana\n3. Orange", false);
    backdate_choices_opened_at(&mut a);
    a.choices_navigated = true;

    let r = handle_key(&mut a, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));

    assert_eq!(r, None);
    assert!(
        !a.choices.is_empty(),
        "an already-navigated picker must not be dismissed by Down"
    );
    assert_eq!(
        a.choice_cursor, 1,
        "Down must still move the cursor once navigation has started"
    );
}

/// Why (#3346): `dismiss_choices` is the single reset point every dismiss
/// site relies on; if it ever forgets a field, a later fresh picker could
/// inherit stale `choices_navigated` / `choices_opened_at` state from a
/// previous one and skip the history-recall disambiguation incorrectly.
/// What: Populate every picker-related field to a non-default value, call
/// `dismiss_choices`, and assert all five reset to their empty defaults.
/// Test: `repl_app_dismiss_choices_resets_all_picker_state` (this test).
#[test]
fn repl_app_dismiss_choices_resets_all_picker_state() {
    let mut a = ReplApp::new("ctrl".into(), "u".into());
    a.choices = vec!["one".to_string(), "two".to_string()];
    a.choice_cursor = 1;
    a.choices_context = Some("switch".to_string());
    a.choices_navigated = true;
    a.choices_opened_at = Some(std::time::Instant::now());

    a.dismiss_choices();

    assert!(a.choices.is_empty());
    assert_eq!(a.choice_cursor, 0);
    assert!(a.choices_context.is_none());
    assert!(!a.choices_navigated);
    assert!(a.choices_opened_at.is_none());
}

/// Why (#3346): The history-recall disambiguation must not regress a live
/// slash-completion picker (e.g. typing `/s` → `/status`/`/switch`/…) —
/// those are always shown with a non-empty, `/`-prefixed input buffer, so
/// `should_navigate_picker` must keep letting Up/Down browse them exactly as
/// before, never reinterpreting the arrow key as history recall.
/// What: Type `/s` (which matches several commands, so the cursor has room
/// to move) to build the live completion list, then press `Down`. Assert
/// `choice_cursor` moved and the completion list is still showing (not
/// dismissed, not swallowed into `history_next`).
/// Test: `repl_app_live_slash_picker_up_down_still_navigates` (this test).
#[test]
fn repl_app_live_slash_picker_up_down_still_navigates() {
    let mut a = ReplApp::new("ctrl".into(), "u".into());
    handle_key(
        &mut a,
        KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
    );
    handle_key(
        &mut a,
        KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE),
    );
    assert!(
        a.choices.len() > 1,
        "setup needs 2+ matches to observe cursor movement, got {:?}",
        a.choices
    );

    let r = handle_key(&mut a, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));

    assert_eq!(r, None);
    assert_eq!(a.input_buf, "/s", "typed buffer must be untouched");
    assert!(
        !a.choices.is_empty(),
        "a live slash-completion list must never be dismissed by Down"
    );
    assert_eq!(
        a.choice_cursor, 1,
        "Down must navigate the live slash-completion list as before"
    );
}
