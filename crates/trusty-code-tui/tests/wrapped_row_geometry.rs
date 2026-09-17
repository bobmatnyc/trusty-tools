//! #8205 regression: the row count that SIZES the chat pane must equal the
//! rows ratatui then RENDERS into it.
//!
//! Why this lives outside `widgets::scrollback` rather than beside the
//! counter: every assertion here goes through the crate's public surface
//! (`build_chat_lines`, `chat_line_count`, `banner_lines`, `layout::draw`), so
//! the whole file compiles and runs against the pre-fix commit too. That is
//! what lets it be shown failing before the fix instead of merely passing
//! after it.
//!
//! What: each case renders the very `Paragraph` the pane renders — same lines,
//! same `Wrap { trim: false }`, same width — into a `TestBackend`, and asks
//! where it ended. No expected row count is hand-computed anywhere in this
//! file; the renderer is the oracle.

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Wrap};
use trusty_code_tui::app::{ChatLine, ChatRole, ReplApp};
use trusty_code_tui::model::{CommandDescriptor, CommandRouting};
use trusty_code_tui::widgets::banner::banner_lines;
use trusty_code_tui::widgets::scrollback::{build_chat_lines, chat_line_count};

/// A short marker appended after the lines under test; the buffer row it lands
/// on IS the number of rows those lines consumed.
const SENTINEL: &str = "@@@";

/// Rows `lines` really occupy at `width`, read off a rendered buffer.
///
/// Counting non-blank rows would miss a trailing blank line and a wrapped row
/// that happens to hold only spaces, so the marker is rendered after them and
/// its row index is the answer.
fn really_rendered_rows(lines: &[Line<'static>], width: u16) -> usize {
    let mut all = lines.to_vec();
    all.push(Line::from(SENTINEL));
    // Tall enough for the marker under any wrapping, and no taller: a fixed
    // ceiling makes every case pay for the widest one.
    let cells: usize = all.iter().map(|l| l.width()).sum();
    let height = u16::try_from(cells / width as usize + all.len() + 2).unwrap_or(u16::MAX);
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("construct terminal");
    terminal
        .draw(|f| {
            let paragraph = Paragraph::new(all.clone()).wrap(Wrap { trim: false });
            f.render_widget(paragraph, f.area());
        })
        .expect("draw");
    let buffer = terminal.backend().buffer().clone();
    let row = |y: u16| -> String {
        (0..width)
            .map(|x| buffer[(x, y)].symbol().to_string())
            .collect()
    };
    (0..height)
        .position(|y| row(y).contains(SENTINEL))
        .expect("the marker must fit in the rendered buffer")
}

/// The banner alone, carrying the `format!("   /{:<12} - {}")` command column
/// whose padding is the run of interior spaces `split_whitespace` collapsed.
fn banner_app() -> ReplApp {
    let mut app = ReplApp::new("demo", "bob");
    app.commands = vec![
        CommandDescriptor {
            name: "workstream".to_string(),
            summary: "List or activate a workstream".to_string(),
            routing: CommandRouting::Engine,
            args_hint: None,
        },
        CommandDescriptor {
            name: "q".to_string(),
            summary: "Quit".to_string(),
            routing: CommandRouting::Engine,
            args_hint: None,
        },
    ];
    app
}

/// One chat entry rendered on its own, with no banner in the way.
fn app_with(role: ChatRole, text: &str) -> ReplApp {
    let mut app = ReplApp::new("demo", "bob");
    app.show_banner = false;
    app.chat.push(ChatLine {
        role,
        text: text.to_string(),
        tool: None,
    });
    app
}

/// Every chat shape the pre-#8205 `chars()`-and-`split_whitespace()` wrapper
/// measured wrong, plus the ones it got right, against a real render.
///
/// A CJK glyph is one `char` and two columns, so a wide line measured half its
/// rows. A run of two or more interior spaces is ONE separator to
/// `split_whitespace` and N columns to the renderer, so a padded column
/// measured short. Both under-count: the pane came up short and ratatui
/// dropped the tail, which is the #8205 defect. A leading indent — the
/// assistant continuation's three spaces, the delegation gutter — was charged
/// to the row AND again as a separator before the first word, which
/// over-counts at whichever width puts that first row exactly at the boundary;
/// that costs a blank row at the pane's bottom and a scroll cap one too high.
/// The indented shapes are therefore swept across widths rather than probed at
/// one: the off-by-one only shows where the boundary lands.
#[test]
fn row_count_agrees_with_a_real_render() {
    let long = ["123456789"; 12].join(" ");
    let mut cases: Vec<(String, ReplApp, u16)> = vec![
        (
            "interior whitespace runs".to_string(),
            app_with(ChatRole::Status, "ab  cd  ef  gh  ij  kl"),
            12,
        ),
        (
            "cjk wide characters".to_string(),
            app_with(ChatRole::Status, &"中".repeat(60)),
            80,
        ),
        (
            "plain long line".to_string(),
            app_with(ChatRole::Status, &"x".repeat(200)),
            80,
        ),
        (
            "the 80-column connect line".to_string(),
            app_with(
                ChatRole::Status,
                "connected to tcode daemon at \
                 /Users/dev0/Library/Application Support/tcode/tcode.sock — home \
                 /private/tmp/q8230/repoA, solo agent (no delegation)",
            ),
            80,
        ),
    ];
    for width in 20u16..=100 {
        cases.push((
            "assistant continuation indent".to_string(),
            app_with(ChatRole::Assistant, &format!("lead\n{long}")),
            width,
        ));
        cases.push((
            "delegated gutter indent".to_string(),
            app_with(ChatRole::Delegated, &long),
            width,
        ));
    }
    for width in [20u16, 40, 60, 80, 100] {
        cases.push(("the banner command column".to_string(), banner_app(), width));
    }

    // Every shape is reported, not just the first — the point of the case
    // list is to name each miscount, and stopping at case one hides the rest.
    let mut wrong: Vec<String> = Vec::new();
    for (name, app, width) in cases {
        let lines = build_chat_lines(&app, width as usize);
        let predicted = chat_line_count(&app, width as usize);
        let rendered = really_rendered_rows(&lines, width);
        if predicted != rendered {
            wrong.push(format!(
                "{name} at {width} columns: sized for {predicted} rows, renders {rendered}"
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "the pane is sized by this count, so it must equal the rendered rows: {wrong:#?}"
    );
}

/// #8205: the whole banner must reach the screen — BOTH rules, at every width.
///
/// The banner-only branch of `layout::draw` sized the pane with
/// `banner_lines(..).len()`, which is short two different ways, and the pane
/// scrolls to its bottom when content overflows, so what a short pane loses is
/// the TOP rule. Below 40 columns `banner_lines` builds wider than the pane and
/// every row wraps, so the count is about half the rows needed; at any width it
/// omitted the blank row `build_chat_lines` appends after the banner, which is
/// a one-row scroll on its own. 80 columns is in the list for exactly that.
#[test]
fn the_banner_renders_whole_below_its_build_width() {
    for width in [20u16, 30, 39, 80] {
        let app = ReplApp::new("demo", "bob");
        assert!(app.show_banner, "this is the banner-only branch of `draw`");
        let lines = banner_lines(&app, width as usize);
        let needed = really_rendered_rows(&lines, width);
        assert!(
            needed >= lines.len(),
            "a wrapped banner can never need fewer rows than it has lines"
        );

        let backend = TestBackend::new(width, 80);
        let mut terminal = Terminal::new(backend).expect("construct terminal");
        terminal
            .draw(|f| trusty_code_tui::layout::draw(f, &app))
            .expect("draw");
        let buffer = terminal.backend().buffer().clone();
        let rendered: Vec<String> = (0..80u16)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect()
            })
            .collect();
        for rule in ['╭', '╰'] {
            assert!(
                rendered.iter().any(|r| r.contains(rule)),
                "the banner's {rule} rule must render at {width} columns \
                 (the banner needs {needed} rows): {rendered:#?}"
            );
        }
    }
}
