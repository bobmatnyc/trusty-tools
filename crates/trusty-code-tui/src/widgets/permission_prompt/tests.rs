//! Rendered-output tests for `super::permission_prompt_lines` (#3422): the
//! prompt draws into a `TestBackend`, so each assertion reads the cells a
//! terminal would show.

use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::{ANSWER_HINTS, PROMPT_HEIGHT, draw_permission_prompt, permission_prompt_lines};
use crate::app::{ReplApp, apply};
use crate::event::ReplEvent;
use crate::model::PendingPermission;

const WIDTH: u16 = 72;

fn pending(subject: &str) -> PendingPermission {
    PendingPermission {
        request_id: "req-1".into(),
        agent: "python-engineer".into(),
        agent_id: "spawn-1".into(),
        tool: "bash".into(),
        subject: subject.into(),
        rule: "bash[rm *]".into(),
    }
}

/// Render the prompt into a `WIDTH`x[`PROMPT_HEIGHT`] buffer and return its
/// rows, trailing blanks trimmed.
fn rows(app: &ReplApp) -> Vec<String> {
    let mut terminal =
        Terminal::new(TestBackend::new(WIDTH, PROMPT_HEIGHT)).expect("construct terminal");
    terminal
        .draw(|f| draw_permission_prompt(f, app, f.area()))
        .expect("draw prompt");
    let buf = terminal.backend().buffer();
    (0..PROMPT_HEIGHT)
        .map(|y| {
            (0..WIDTH)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect()
}

fn app_with_prompt(subject: &str) -> ReplApp {
    let mut app = ReplApp::new("tcode", "bob");
    let p = pending(subject);
    apply(
        &mut app,
        ReplEvent::PermissionRequested {
            request_id: p.request_id,
            agent: p.agent,
            agent_id: p.agent_id,
            tool: p.tool,
            subject: p.subject,
            rule: p.rule,
        },
    );
    app
}

/// The prompt must name all three facts the user decides on: who wants what,
/// what it would act on, and which rule stopped it. A prompt missing any one
/// of them asks the user to approve something they cannot see.
#[test]
fn prompt_names_the_tool_subject_and_rule() {
    let app = app_with_prompt("rm -rf build");
    let rendered = rows(&app).join("\n");
    assert!(rendered.contains("python-engineer"), "{rendered}");
    assert!(rendered.contains("bash"), "{rendered}");
    assert!(rendered.contains("rm -rf build"), "{rendered}");
    assert!(rendered.contains("bash[rm *]"), "{rendered}");
}

/// Every answer the reducer binds must be advertised, or a user has no way to
/// discover the third option at all.
#[test]
fn prompt_advertises_every_answer_key() {
    let app = app_with_prompt("rm -rf build");
    let rendered = rows(&app).join("\n");
    for (key, label) in ANSWER_HINTS {
        assert!(rendered.contains(&format!("[{key}]")), "{key}: {rendered}");
        assert!(rendered.contains(label), "{label}: {rendered}");
    }
}

/// A tool with no nameable subject (an MCP call) still gets a fixed-height
/// prompt — a height that varies with the request would shift the input
/// composer under the user's cursor.
#[test]
fn prompt_with_an_empty_subject_keeps_its_height() {
    assert_eq!(
        permission_prompt_lines(&pending("")).len(),
        PROMPT_HEIGHT as usize
    );
    assert_eq!(
        permission_prompt_lines(&pending("a\nb\nc")).len(),
        PROMPT_HEIGHT as usize
    );
}

/// A multi-line subject (several file paths) folds onto one row, for the same
/// fixed-height reason.
#[test]
fn prompt_folds_a_multi_line_subject_onto_one_row() {
    let app = app_with_prompt("src/a.rs\nsrc/b.rs");
    let rendered = rows(&app);
    let subject_row = &rendered[1];
    assert!(subject_row.contains("src/a.rs · src/b.rs"), "{subject_row}");
}

/// With no prompt open the widget draws nothing — the layout only reserves
/// rows while one is pending, and a stale render would outlive the request.
#[test]
fn no_prompt_draws_nothing() {
    let app = ReplApp::new("tcode", "bob");
    assert!(rows(&app).iter().all(|r| r.is_empty()));
}
