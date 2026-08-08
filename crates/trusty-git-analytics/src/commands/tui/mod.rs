//! `tga tui` — an interactive terminal view over collection and correlation.
//!
//! Why: `tga collect` and `tga classify` are minutes-long batch commands whose
//! only progress surface is an `indicatif` bar that is hidden off-TTY, and the
//! commit ↔ board-item correlation stored in `commit_work_items` had no view at
//! all. #5197 adds one screen that picks repositories from the existing config,
//! shows the pipelines running live off the [`tga::core::progress`] bus, and
//! reports which commits reached the board and which did not.
//!
//! What: a subcommand of the existing `tga` binary (not a new crate), built on
//! ratatui 0.29 / crossterm 0.28 — the versions already in the workspace
//! dependency table and the ones `trusty_common`'s `monitor-tui` feature uses,
//! so there is exactly one ratatui major in the build graph. The file split
//! follows the house pattern in `trusty_common::monitor::search_tui`:
//! [`state`] holds everything drawn, [`render`] draws it, [`event_loop`] maps
//! keys, and [`work`] owns the database and the background runs.
//!
//! **Zero inference is the default.** Every view, every pull, and the whole
//! correlation pass are deterministic: no model is consulted and no API key is
//! read anywhere in this module or in [`tga::collect::correlate`]. `[c]` runs
//! the correlation pass with no network at all. Inference stays where it
//! already is — the classify cascade's opt-in tier 4 — and switching it off
//! changes nothing here.
//!
//! Terminal hygiene is a correctness requirement: raw mode and the alternate
//! screen are restored on normal exit, on error, and on panic. See
//! [`install_panic_hook`].
//!
//! Test: `tests` in this module covers the picker, the key map, the state
//! transitions, and the zero-credential path.

mod event_loop;
mod render;
mod state;
mod work;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

use tga::core::config::Config;
use tga::core::db::Database;
use trusty_common::monitor::tui_common::{enter_tui, leave_tui};

use crate::commands::args::TuiArgs;
use state::TuiState;
use work::WorkerHandle;

/// Restore the terminal from raw mode and the alternate screen.
///
/// Why: `leave_tui` needs a live `Terminal`, which a panicking thread has
/// already given up. This talks to stdout directly so the same restoration
/// works from a panic hook.
/// What: disables raw mode, releases mouse capture, leaves the alternate
/// screen, and shows the cursor. Every step is best-effort — a terminal that
/// is already restored must not turn this into a second failure.
/// Test: side-effect-only terminal glue.
fn restore_terminal() {
    use crossterm::{
        cursor::Show,
        event::DisableMouseCapture,
        execute,
        terminal::{disable_raw_mode, LeaveAlternateScreen},
    };
    let _ = disable_raw_mode();
    let _ = execute!(
        std::io::stdout(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        Show
    );
}

/// Wrap the current panic hook so a panic never leaves the terminal wedged.
///
/// Why: a panic inside the draw loop unwinds past every `leave_tui` call, and
/// an operator left in raw mode on the alternate screen has a shell that does
/// not echo and does not scroll. That is a defect, not a rough edge.
/// What: installs a hook that restores the terminal first, then delegates to
/// whatever hook was installed before (so the backtrace still prints).
/// Test: side-effect-only; verified by panicking inside the loop by hand.
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        previous(info);
    }));
}

/// Run `tga tui`.
///
/// Why: the single entry point `main.rs` dispatches to.
/// What: builds the state from `config`, opens a read connection at
/// `db_path`, then hands the terminal to a blocking task so the draw loop
/// never occupies an async worker while the pipelines run on the same runtime.
/// The terminal is restored on every exit path — normal, error, and panic.
///
/// # Errors
///
/// Returns an error when the database cannot be opened, when terminal setup
/// fails, or when the loop itself fails. The terminal is restored first in
/// every one of those cases.
pub async fn run(config: Config, db_path: &Path, args: TuiArgs) -> anyhow::Result<()> {
    let db_path: PathBuf = db_path.to_path_buf();
    tokio::task::spawn_blocking(move || run_blocking(config, db_path, args)).await?
}

/// The blocking half of [`run`]: terminal setup, loop, teardown.
///
/// Why: split out so the `spawn_blocking` closure stays a one-liner and the
/// teardown ordering is readable.
/// What: installs the panic hook, opens the database, enters the alternate
/// screen, runs the loop, and restores the terminal before returning either
/// result.
/// Test: side-effect-only glue.
fn run_blocking(config: Config, db_path: PathBuf, args: TuiArgs) -> anyhow::Result<()> {
    install_panic_hook();

    let db = Database::open(&db_path)?;
    let mut state = TuiState::new(config).offline(args.correlate_only);
    let mut worker = WorkerHandle::new(db, db_path);
    worker.reload_results(&mut state);

    let mut terminal = enter_tui()?;
    let result = event_loop::run_loop(&mut terminal, &mut state, &mut worker);
    // Restore on BOTH paths, and prefer the loop's error over a teardown one.
    let teardown = leave_tui(&mut terminal);
    restore_terminal();
    result.and(teardown)
}
