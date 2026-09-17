//! Rendered-output tests for `super::tool_card_lines` (#4596): events go
//! through the real reducer and the chat pane draws into a `TestBackend`, so
//! each assertion reads the cells a terminal would show.

use ratatui::Terminal;
use ratatui::backend::TestBackend;

use crate::app::{ReplApp, apply};
use crate::event::ReplEvent;
use crate::widgets::scrollback::{DELEGATED_GUTTER, draw_chat};

const WIDTH: u16 = 60;
const HEIGHT: u16 = 16;

/// Draw the chat pane into a `WIDTH`x`HEIGHT` buffer and return its rows,
/// trailing blanks trimmed.
fn rows(app: &ReplApp) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).expect("terminal");
    terminal
        .draw(|f| draw_chat(f, app, f.area()))
        .expect("draw chat pane");
    let buf = terminal.backend().buffer();
    (0..HEIGHT)
        .map(|y| {
            (0..WIDTH)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect()
}

fn tool(id: &str, agent_id: &str, name: &str, args: &str, result: Option<&str>) -> ReplEvent {
    ReplEvent::ToolInvocation {
        id: id.into(),
        agent_id: agent_id.into(),
        tool_name: name.into(),
        // A completion carries `Null` args, as trusty-code's producer sends.
        args: if result.is_some() {
            serde_json::Value::Null
        } else {
            serde_json::json!(args)
        },
        result: result.map(str::to_string),
    }
}

fn app() -> ReplApp {
    let mut app = ReplApp::new("demo", "u");
    app.show_banner = false;
    app
}

/// Ctrl-O, the expand/collapse toggle (#4596).
fn toggle() -> ReplEvent {
    ReplEvent::Key(crate::event::KeyInput {
        code: crate::event::KeyCode::Char('o'),
        modifiers: crate::event::KeyModifiers {
            ctrl: true,
            alt: false,
            shift: false,
        },
    })
}

/// The start event draws a pending card; its completion fills that same card
/// rather than drawing a second entry.
#[test]
fn tool_call_and_result_render_as_one_card() {
    let mut app = app();
    apply(&mut app, tool("c1", "", "git.checkout", "main", None));
    let pending = rows(&app);
    assert!(
        pending.contains(&"⏺ git.checkout(main)".to_string()),
        "{pending:#?}"
    );
    assert!(
        pending.contains(&"  ⎿  running…".to_string()),
        "{pending:#?}"
    );

    apply(
        &mut app,
        tool("c1", "", "git.checkout", "", Some("switched to main")),
    );
    apply(&mut app, toggle()); // expand the now-collapsed card (#4596)
    let done = rows(&app);
    let headers = done.iter().filter(|r| r.contains("git.checkout")).count();
    assert_eq!(headers, 1, "one card, not two: {done:#?}");
    let at = done
        .iter()
        .position(|r| r == "⏺ git.checkout(main)")
        .expect("header keeps the call's args");
    assert_eq!(done[at + 1], "  ⎿  switched to main", "{done:#?}");
    assert!(!done.iter().any(|r| r.contains("running…")), "{done:#?}");
}

/// #4596: a call that succeeded draws as ONE row — name, args, status and
/// the hidden line count — not as its whole result body.
#[test]
fn completed_card_collapses_to_a_one_line_summary() {
    let mut app = app();
    apply(&mut app, tool("r1", "", "read_file", "src/lib.rs", None));
    let body = "pub fn add(a: i64, b: i64) -> i64 {\n    a + b\n}";
    apply(&mut app, tool("r1", "", "read_file", "", Some(body)));

    let out = rows(&app);
    assert!(
        out.contains(&"⏺ read_file(src/lib.rs) · ok · 3 lines".to_string()),
        "{out:#?}"
    );
    assert!(
        !out.iter().any(|r| r.contains("pub fn add")),
        "the body must be hidden while collapsed: {out:#?}"
    );
}

/// #4596: the toggle round-trips — Ctrl-O shows the full body, Ctrl-O again
/// returns the one-line summary.
#[test]
fn toggle_round_trips_between_summary_and_full_body() {
    let mut app = app();
    apply(&mut app, tool("r1", "", "read_file", "src/lib.rs", None));
    apply(&mut app, tool("r1", "", "read_file", "", Some("a\nb")));
    assert!(rows(&app).iter().any(|r| r.contains("· ok · 2 lines")));

    apply(&mut app, toggle());
    let open = rows(&app);
    assert!(open.contains(&"  ⎿  a".to_string()), "{open:#?}");
    assert!(open.contains(&"     b".to_string()), "{open:#?}");
    assert!(
        !open.iter().any(|r| r.contains("· ok ·")),
        "expanded header drops the summary suffix: {open:#?}"
    );

    apply(&mut app, toggle());
    let shut = rows(&app);
    assert!(
        shut.iter().any(|r| r.contains("· ok · 2 lines")),
        "{shut:#?}"
    );
    assert!(!shut.contains(&"  ⎿  a".to_string()), "{shut:#?}");
}

/// #4596: a failed call is expanded from the start — its error text is the
/// one thing that must never need a keystroke to read.
#[test]
fn errored_card_renders_expanded_by_default() {
    let mut app = app();
    apply(&mut app, tool("e1", "", "read_file", "missing.rs", None));
    apply(
        &mut app,
        tool("e1", "", "read_file", "", Some("ERROR: no such file")),
    );
    let out = rows(&app);
    assert!(
        out.contains(&"  ⎿  ERROR: no such file".to_string()),
        "the error body must be visible without a keystroke: {out:#?}"
    );
    assert!(
        !out.iter().any(|r| r.contains("· ok")),
        "an errored card is not summarized as ok: {out:#?}"
    );
}

/// Each line of a multi-line result lands on its own row, in order, once
/// the card is expanded.
#[test]
fn multi_line_result_keeps_its_line_breaks() {
    let mut app = app();
    apply(&mut app, tool("r1", "", "read_file", "src/lib.rs", None));
    let body = "pub fn add(a: i64, b: i64) -> i64 {\n    a + b\n}";
    apply(&mut app, tool("r1", "", "read_file", "", Some(body)));
    apply(&mut app, toggle());
    let out = rows(&app);
    let at = out
        .iter()
        .position(|r| r == "  ⎿  pub fn add(a: i64, b: i64) -> i64 {")
        .unwrap_or_else(|| panic!("first result line on its own row: {out:#?}"));
    assert_eq!(out[at + 1], "         a + b", "{out:#?}");
    assert_eq!(out[at + 2], "     }", "{out:#?}");
}

/// A delegated sub-agent's card carries the delegation gutter on every row;
/// the primary agent's card stays flush left at top level (#7940).
#[test]
fn delegated_card_is_guttered_and_top_level_card_is_not() {
    let mut app = app();
    apply(
        &mut app,
        ReplEvent::DelegationStarted {
            agent_id: "eng-1".into(),
            agent: "engineer".into(),
            task: "run tests".into(),
        },
    );
    apply(&mut app, tool("d1", "eng-1", "bash", "cargo test", None));
    apply(
        &mut app,
        tool("d1", "eng-1", "bash", "", Some("ok\n2 passed")),
    );
    // #4596: expand it while it is still the newest card, so the gutter is
    // asserted against every body row and not just the summary.
    apply(&mut app, toggle());
    apply(
        &mut app,
        tool("p1", "pm-1", "delegate_to_agent", "engineer", None),
    );
    let out = rows(&app);

    for expected in ["⏺ bash(cargo test)", "  ⎿  ok", "     2 passed"] {
        let row = format!("{DELEGATED_GUTTER}{expected}");
        assert!(
            out.contains(&row),
            "missing delegated row {row:?}: {out:#?}"
        );
    }
    assert!(
        out.contains(&"⏺ delegate_to_agent(engineer)".to_string()),
        "top-level card must be flush left: {out:#?}"
    );
}
