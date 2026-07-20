//! Regression tests for #3346: disambiguating a first Up/Down keypress over
//! `app.choices` between picker navigation and shell-style history recall.
//! Kept in their own file (new, not split out of anything) so the picker/
//! history interaction has one focused home distinct from the general
//! input-editing coverage in `tests_input.rs`. Flat re-exports in `mod.rs`
//! make every item reachable via `super::*`.

#![cfg(test)]

use super::*;

/// Why (#3346): The #3325 fix dismissed a stale picker on any key EXCEPT
/// Up/Down/Enter/Esc, leaving Up/Down as first keystroke over a leftover,
/// untouched, untagged `detect_choices` list still captured as picker
/// navigation instead of the shell-style history recall the user almost
/// certainly wanted (empty input buffer = nothing typed toward a picker
/// interaction yet).
/// What: Populate `app.choices` the way `push_assistant` -> `detect_choices`
/// does (untagged, `choices_context.is_none()`), leave the input buffer
/// empty, and send a single `Up`. Assert the picker is dismissed AND the
/// normal Up-arrow history-recall path ran (`last_prompt` copied into
/// `input_buf`), exactly like `repl_app_up_arrow_recalls_last_prompt` with
/// no picker in the way.
/// Test: `repl_app_first_up_over_stale_untagged_picker_recalls_history` (this test).
#[test]
fn repl_app_first_up_over_stale_untagged_picker_recalls_history() {
    let mut a = ReplApp::new("ctrl".into(), "u".into());
    a.push_assistant("Pick one:\n1. Apple\n2. Banana\n3. Orange", false);
    assert!(!a.choices.is_empty(), "setup: picker must be showing");
    assert!(a.choices_context.is_none(), "setup: list must be untagged");
    assert!(a.input_buf.is_empty(), "setup: buffer must be empty");
    a.last_prompt = "earlier prompt".to_string();

    let r = handle_key(&mut a, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));

    assert_eq!(r, None);
    assert!(
        a.choices.is_empty(),
        "first Up over an untouched untagged picker with an empty buffer \
         must dismiss it"
    );
    assert_eq!(
        a.input_buf, "earlier prompt",
        "the Up keypress must fall through to normal history recall"
    );
}

/// Why (#3346): Mirrors `repl_app_first_up_over_stale_untagged_picker_recalls_history`
/// for `Down`.
/// What: Populate an untagged `app.choices` list directly, leave the buffer
/// empty, seed `history` + `history_idx` so `history_next` has somewhere to
/// go, and send a single `Down`. Assert the picker is dismissed AND
/// `history_next` ran.
/// Test: `repl_app_first_down_over_stale_untagged_picker_recalls_history` (this test).
#[test]
fn repl_app_first_down_over_stale_untagged_picker_recalls_history() {
    let mut a = ReplApp::new("ctrl".into(), "u".into());
    a.choices = vec![
        "Apple".to_string(),
        "Banana".to_string(),
        "Other (type your own)".to_string(),
    ];
    a.choice_cursor = 0;
    a.choices_context = None;
    a.history = vec!["cmd one".to_string(), "cmd two".to_string()];
    a.history_idx = Some(0);
    assert!(a.input_buf.is_empty(), "setup: buffer must be empty");

    let r = handle_key(&mut a, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));

    assert_eq!(r, None);
    assert!(
        a.choices.is_empty(),
        "first Down over an untouched untagged picker with an empty buffer \
         must dismiss it"
    );
    assert_eq!(
        a.input_buf, "cmd two",
        "the Down keypress must fall through to normal history_next"
    );
}

/// Why (#3346, CRITICAL fix from code-critic review of the first version of
/// this PR): `/switch`'s persona list has no input mechanism other than
/// Up/Down + Enter — the user opened it on purpose and is reading persona
/// names before pressing a key. An earlier version of this fix used a
/// wall-clock staleness window and dismissed the picker (recalling history
/// instead) if the user paused more than ~2s before their first Down. That
/// is a real, common interaction (read the list, then act) and made
/// `/switch` unusable after any brief pause. `/switch` must always navigate,
/// unconditionally, on the very first press, with no time dependency at
/// all — `should_navigate_picker` special-cases `choices_context.is_some()`
/// specifically so this can never regress again.
/// What: Populate `app.choices` the way the `/switch` handler does
/// (`choices_context = Some("switch")`), leave the buffer empty, and send a
/// `Down` as the very first key. Assert `choice_cursor` moved, the picker
/// survived, and neither history recall nor `input_buf` was touched.
/// Test: `repl_app_switch_picker_up_down_always_navigates` (this test).
#[test]
fn repl_app_switch_picker_up_down_always_navigates() {
    let mut a = ReplApp::new("ctrl".into(), "u".into());
    a.choices = vec![
        "ctrl".to_string(),
        "Izzie".to_string(),
        "CTO Assistant".to_string(),
    ];
    a.choice_cursor = 0;
    a.choices_context = Some("switch".to_string());
    a.last_prompt = "should not be recalled".to_string();

    let r = handle_key(&mut a, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));

    assert_eq!(r, None);
    assert!(
        !a.choices.is_empty(),
        "/switch's picker must never be dismissed by Down, even as the \
         very first keypress"
    );
    assert_eq!(
        a.choice_cursor, 1,
        "Down must navigate the /switch picker on the first press"
    );
    assert!(
        a.input_buf.is_empty(),
        "navigating /switch must never touch input_buf via history recall"
    );
}

/// Why (#3346): Untagged `detect_choices` lists are still arrow-navigable by
/// design (`draw_inline_choice_picker` highlights `choice_cursor`, and Enter
/// inserts the highlighted item into `input_buf`) — the history-recall
/// carve-out must only intercept the picker's very first, untouched
/// Up/Down. Once the user has genuinely started navigating it, every
/// further press must keep navigating regardless of the buffer.
/// What: Populate an untagged list, mark it as already-navigated
/// (`choices_navigated = true`, simulating a prior Down/Up press this
/// session), and send a `Down`. Assert `choice_cursor` moved and the picker
/// survived instead of being reinterpreted as history recall.
/// Test: `repl_app_untagged_picker_navigates_once_started` (this test).
#[test]
fn repl_app_untagged_picker_navigates_once_started() {
    let mut a = ReplApp::new("ctrl".into(), "u".into());
    a.push_assistant("Pick one:\n1. Apple\n2. Banana\n3. Orange", false);
    a.choices_navigated = true;

    let r = handle_key(&mut a, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));

    assert_eq!(r, None);
    assert!(
        !a.choices.is_empty(),
        "an already-navigated untagged picker must not be dismissed by Down"
    );
    assert_eq!(
        a.choice_cursor, 1,
        "Down must still move the cursor once navigation has started"
    );
}

/// Why (#3346): `dismiss_choices` is the single reset point every dismiss
/// site relies on; if it ever forgets a field, a later fresh picker could
/// inherit stale `choices_navigated` state from a previous one and skip the
/// history-recall disambiguation incorrectly.
/// What: Populate every picker-related field to a non-default value, call
/// `dismiss_choices`, and assert all four reset to their empty defaults.
/// Test: `repl_app_dismiss_choices_resets_all_picker_state` (this test).
#[test]
fn repl_app_dismiss_choices_resets_all_picker_state() {
    let mut a = ReplApp::new("ctrl".into(), "u".into());
    a.choices = vec!["one".to_string(), "two".to_string()];
    a.choice_cursor = 1;
    a.choices_context = Some("switch".to_string());
    a.choices_navigated = true;

    a.dismiss_choices();

    assert!(a.choices.is_empty());
    assert_eq!(a.choice_cursor, 0);
    assert!(a.choices_context.is_none());
    assert!(!a.choices_navigated);
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
