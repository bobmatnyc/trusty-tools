//! #11947: the new-session picker draws its highlight at every terminal height.
//!
//! Why: the overlay is 60% of the terminal tall, but the list window was a
//! fixed eight rows, so under ~20 rows the tail of the window was clipped and
//! the highlight could move onto a row that was never drawn — the arrows
//! looked dead (#11822).
//! What: every test renders the real frame through a `TestBackend` and reads
//! the highlight (`▸`) and the drawn rows back from the screen, so the window
//! arithmetic and the drawn height are checked together.

use ratatui::{Terminal, backend::TestBackend};
use trusty_mpm::project::Project;

use super::new_session::{self, NewSessionFlow, ProjectIdentity};
use super::render;
use super::state::{Input, TuiState};

/// Registered projects in the list; the escape row makes it one more.
const PROJECTS: usize = 26;

/// The width every test renders at; wide enough that no row is cut.
const WIDTH: u16 = 100;

fn no_identity(_: &str) -> Option<ProjectIdentity> {
    None
}

/// A state with the picker open over [`PROJECTS`] rows named `row00`….
fn picker() -> TuiState {
    let projects: Vec<Project> = (0..PROJECTS)
        .map(|i| {
            serde_json::from_value(serde_json::json!({
                "name": format!("row{i:02}"),
                "repo_url": format!("https://ex.test/acme/row{i:02}"),
                "default_branch": "main",
            }))
            .expect("project fixture")
        })
        .collect();
    let mut state = TuiState::new(None, None);
    state.sync(&[]);
    state.open_new_session(NewSessionFlow::with_resolver(
        new_session::targets_from(&projects, &[]),
        no_identity,
    ));
    state
}

/// The picker row a screen line draws: `row07`, `other`, or `None`.
fn row_id(line: &str) -> Option<String> {
    if line.contains("other —") {
        return Some("other".to_string());
    }
    let at = line.find("acme/row")? + "acme/".len();
    line.get(at..at + 5).map(str::to_string)
}

/// Render one frame at `height` rows; return the drawn rows and the highlight.
fn frame(height: u16, state: &mut TuiState) -> (Vec<String>, Option<String>) {
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, height)).expect("test terminal");
    terminal
        .draw(|f| render::render(f, &[], state))
        .expect("render must not panic");
    let buffer = terminal.backend().buffer().clone();
    let lines: Vec<String> = (0..height)
        .map(|y| (0..WIDTH).map(|x| buffer[(x, y)].symbol()).collect())
        .collect();
    let rows = lines.iter().filter_map(|l| row_id(l)).collect();
    let highlight = lines
        .iter()
        .find(|l| l.contains('▸'))
        .and_then(|l| row_id(l));
    (rows, highlight)
}

/// The id of list position `i`: `row00`…, then the escape row.
fn id(i: usize) -> String {
    if i < PROJECTS {
        format!("row{i:02}")
    } else {
        "other".to_string()
    }
}

/// Criterion 1: on a 15-row terminal, walking down the list never loses the
/// highlight off the drawn rows.
#[test]
fn new_session_window_draws_the_highlight_on_a_short_terminal() {
    let mut state = picker();
    for i in 0..=PROJECTS {
        let (rows, highlight) = frame(15, &mut state);
        assert_eq!(
            highlight.as_deref(),
            Some(id(i).as_str()),
            "highlight on position {i} is not drawn at 15 rows; drawn: {rows:?}"
        );
        state.apply(Input::Down, &[]);
    }
}

/// Criterion 2: at every height from 10 to 24 rows the highlight is drawn on
/// the first row, the last row, and while moving across the window's bottom
/// edge (down) and top edge (up) — and crossing an edge scrolls by one row
/// while moving inside the window holds the rows still.
#[test]
fn new_session_window_scrolls_across_both_edges_at_every_height() {
    for height in 10..=24u16 {
        let mut state = picker();
        let (first, highlight) = frame(height, &mut state);
        assert_eq!(highlight.as_deref(), Some("row00"), "height {height}");
        assert!(!first.is_empty(), "height {height}: no rows drawn");
        let window = first.len();

        // Down to the bottom edge: the rows hold still.
        for i in 1..window {
            state.apply(Input::Down, &[]);
            let (rows, highlight) = frame(height, &mut state);
            assert_eq!(highlight, Some(id(i)), "height {height}, down to {i}");
            assert_eq!(rows, first, "height {height}: scrolled inside the window");
        }
        // One more Down crosses the bottom edge: the rows scroll by one.
        state.apply(Input::Down, &[]);
        let (rows, highlight) = frame(height, &mut state);
        assert_eq!(highlight, Some(id(window)), "height {height}, past bottom");
        assert_eq!(rows.first(), Some(&id(1)), "height {height}: {rows:?}");
        assert_eq!(rows.len(), window, "height {height}: window changed size");

        // Up inside the window: the rows hold still; then cross the top edge.
        let scrolled = rows;
        for i in (1..window).rev() {
            state.apply(Input::Up, &[]);
            let (rows, highlight) = frame(height, &mut state);
            assert_eq!(highlight, Some(id(i)), "height {height}, up to {i}");
            assert_eq!(rows, scrolled, "height {height}: scrolled inside the window");
        }
        state.apply(Input::Up, &[]);
        let (rows, highlight) = frame(height, &mut state);
        assert_eq!(highlight.as_deref(), Some("row00"), "height {height}, past top");
        assert_eq!(rows, first, "height {height}: did not scroll back up");

        // The last row: End, then drawn at the bottom of a full window.
        state.apply(Input::End, &[]);
        let (rows, highlight) = frame(height, &mut state);
        assert_eq!(highlight.as_deref(), Some("other"), "height {height}, End");
        assert_eq!(rows.last().map(String::as_str), Some("other"));
        assert_eq!(rows.len(), window, "height {height}: window changed size");
    }
}
