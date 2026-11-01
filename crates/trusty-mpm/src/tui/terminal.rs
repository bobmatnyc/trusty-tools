//! Shared ratatui terminal setup/teardown for every full-screen TUI surface.
//!
//! Why: three surfaces now enter raw mode and the alternate screen — the
//! coordinator chat, the `project_ctl` multipane dashboard, and (#7224) the
//! `tm ls` session TUI. The first two each carried their own private
//! `TerminalGuard` with byte-identical `Drop` bodies. A third copy would make
//! the restore sequence a thing to keep in step across three files instead of
//! one, and leaving a terminal in raw mode is the worst failure any of them can
//! have: the operator's shell stops echoing and Ctrl-C stops working.
//!
//! What: [`TerminalGuard`] is the `Drop`-based safety net (normal return AND
//! panic unwind); [`enter`] builds a ratatui terminal on stdout already in raw
//! mode and on the alternate screen; [`suspend`] hands the real screen back
//! WITHOUT dropping the terminal, so a caller can shell out (a tmux attach,
//! say) and then [`resume`] onto the same terminal value. #10476: only the
//! projects view turns mouse capture on ([`enter_mouse`], [`enable_capture`]),
//! and every teardown path turns it off, because a shell left in capture mode
//! prints escape sequences on every mouse move.
//!
//! Test: `terminal_guard_drop_is_idempotent`,
//! `enter_with_restores_the_terminal_when_the_screen_fails`,
//! `enter_with_leaves_the_terminal_alone_when_raw_mode_fails`,
//! `enter_with_restores_the_terminal_when_capture_fails`,
//! `restore_disables_capture`, `enter_with_mouse_false_never_enables_capture`,
//! `suspend_disables_and_resume_reenables_only_when_asked`,
//! `mouse_enabled_honours_flag_and_env` and
//! `term_supports_raw_mode_rejects_dumb_and_missing` in `super::tests`.

use std::io::{self, Stdout};
use std::sync::atomic::{AtomicBool, Ordering};

use crossterm::{
    cursor::Show,
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};

/// The ratatui terminal type every full-screen surface in this crate uses.
pub type TuiTerminal = Terminal<CrosstermBackend<Stdout>>;

/// True while this process's TUI wants mouse capture, so [`resume`] puts back
/// what [`suspend`] took and a surface that never asked gets nothing (#10476).
static CAPTURE_WANTED: AtomicBool = AtomicBool::new(false);

/// May the projects view capture the mouse?
///
/// Why: capture takes native text selection away, so the operator needs an
/// opt-out (Architect Q11, #10476). Pure, so the rule is testable without
/// touching the process environment.
/// What: `false` for the `--no-mouse` flag or `TM_TUI_MOUSE=0`, else `true`.
/// Test: `mouse_enabled_honours_flag_and_env`.
pub fn mouse_enabled(no_mouse: bool, env: Option<&str>) -> bool {
    !no_mouse && env.map(str::trim) != Some("0")
}

/// [`mouse_enabled`] over this process's `TM_TUI_MOUSE`.
pub fn mouse_from_env(no_mouse: bool) -> bool {
    mouse_enabled(no_mouse, std::env::var("TM_TUI_MOUSE").ok().as_deref())
}

/// Restores cooked mode, the main screen, and the cursor however a TUI exits.
///
/// Why: an early `?` return or a panic unwind would otherwise leave the
/// operator's shell in raw mode on the alternate screen. A `Drop` guard covers
/// both paths that a trailing teardown call misses.
/// What: hold one for the lifetime of the TUI. Its `Drop` is best-effort and
/// idempotent — running it after [`suspend`] has already restored the screen
/// is a no-op in practice, which is what lets a suspend/resume cycle keep the
/// guard installed across the hand-off.
/// Test: `terminal_guard_drop_is_idempotent`.
pub struct TerminalGuard;

impl Drop for TerminalGuard {
    /// Why: see [`TerminalGuard`] — runs on normal return and on panic unwind.
    /// What: best-effort restore of cooked mode, the main screen, and the
    /// cursor; every step ignores its error because a `Drop` cannot propagate
    /// one and a partial restore is still better than none.
    /// Test: side-effect-only teardown; covered by launching a TUI.
    fn drop(&mut self) {
        restore();
    }
}

/// Restore cooked mode, the main screen, and the cursor — best effort.
///
/// Why: the one place the restore sequence is written, so the `Drop` guard and
/// the explicit [`suspend`] can never drift apart on the order of operations.
/// What: `disable_raw_mode`, then [`write_restore`] on stdout; every error is
/// dropped for the reason [`TerminalGuard::drop`] gives.
/// Test: the sequence by `restore_disables_capture`; the raw-mode call is
/// covered by launching a TUI.
fn restore() {
    CAPTURE_WANTED.store(false, Ordering::SeqCst);
    let _ = disable_raw_mode();
    let _ = write_restore(&mut io::stdout());
}

/// Write the teardown sequence: capture off, main screen, cursor shown.
///
/// #10476: capture-off is unconditional. It is a no-op on a terminal that never
/// enabled capture, and a teardown must not depend on state a panic may have
/// left half-written.
/// Test: `restore_disables_capture`.
pub(super) fn write_restore<W: io::Write>(out: &mut W) -> io::Result<()> {
    execute!(out, DisableMouseCapture, LeaveAlternateScreen, Show)
}

/// Write the entry sequence: alternate screen, then capture only if asked.
///
/// Test: `enter_with_mouse_false_never_enables_capture`,
/// `suspend_disables_and_resume_reenables_only_when_asked`.
pub(super) fn write_enter<W: io::Write>(out: &mut W, mouse: bool) -> io::Result<()> {
    execute!(out, EnterAlternateScreen)?;
    if mouse {
        execute!(out, EnableMouseCapture)?;
    }
    Ok(())
}

/// May a full-screen TUI take this `TERM` over, or must the caller print statically?
///
/// Why: a `dumb` (or absent) `TERM` has no cursor addressing, so a redraw smears
/// the frame down the screen instead of repainting it — and raw mode has already
/// taken the operator's echo away by then. One predicate so every gate that
/// guards [`enter`] answers this the same way; a second copy is how `tm ls` and
/// `tm f` came to disagree (#7224).
/// What: true only for a `TERM` that is present, non-empty, and not `dumb`
/// (case-insensitively). `NO_COLOR` is deliberately not consulted: it suppresses
/// color, not cursor addressing.
/// Test: `term_supports_raw_mode_rejects_dumb_and_missing`,
/// `term_supports_raw_mode_accepts_a_real_terminal`.
pub fn term_supports_raw_mode(term: Option<&str>) -> bool {
    matches!(term, Some(t) if !t.is_empty() && !t.eq_ignore_ascii_case("dumb"))
}

/// Enter raw mode + the alternate screen and build the ratatui terminal.
///
/// Why: every caller wants the same four steps in the same order, and getting
/// the order wrong (building the backend before the alternate screen) leaves a
/// frame of the shell's own scrollback painted over.
/// What: `enable_raw_mode`, `EnterAlternateScreen` on stdout, then a
/// `CrosstermBackend` terminal over that same stdout — atomically, so a failure
/// at either later step restores the terminal before propagating (#7224).
/// Callers construct their [`TerminalGuard`] only after this returns `Ok`, so
/// until then this function is the only thing that can undo the raw mode it
/// just took.
/// Test: the atomicity by `enter_with_restores_the_terminal_when_the_screen_fails`
/// and `enter_with_leaves_the_terminal_alone_when_raw_mode_fails`; the real
/// escape sequences are side-effect-only and covered by launching a TUI.
pub fn enter() -> io::Result<TuiTerminal> {
    enter_with(io::stdout(), enable_raw_mode, restore, false)
}

/// [`enter`] for the projects view, which alone may capture the mouse (#10476).
///
/// What: with `mouse` set, also enables capture and arranges for SIGTERM to
/// restore the terminal (`install_term_restore`). With `mouse` unset it is
/// exactly [`enter`]: no enable sequence reaches the terminal.
/// Test: `enter_with_mouse_false_never_enables_capture`,
/// `enter_with_restores_the_terminal_when_capture_fails`.
pub fn enter_mouse(mouse: bool) -> io::Result<TuiTerminal> {
    let terminal = enter_with(io::stdout(), enable_raw_mode, restore, mouse)?;
    if mouse {
        CAPTURE_WANTED.store(true, Ordering::SeqCst);
        install_term_restore();
    }
    Ok(terminal)
}

/// Turn capture on in a terminal another surface entered (#10476).
///
/// Why: the home screen owns the terminal and opens the projects view inside
/// it; the home screen itself must stay uncaptured.
/// What: on failure the error propagates and the caller's [`TerminalGuard`]
/// restores the terminal.
/// Test: the sequence by `suspend_disables_and_resume_reenables_only_when_asked`;
/// the hand-off is a live check.
pub fn enable_capture(terminal: &mut TuiTerminal) -> io::Result<()> {
    execute!(terminal.backend_mut(), EnableMouseCapture)?;
    CAPTURE_WANTED.store(true, Ordering::SeqCst);
    install_term_restore();
    Ok(())
}

/// Turn capture off on leaving the projects view (#10476); see [`enable_capture`].
pub fn disable_capture(terminal: &mut TuiTerminal) -> io::Result<()> {
    CAPTURE_WANTED.store(false, Ordering::SeqCst);
    execute!(terminal.backend_mut(), DisableMouseCapture)
}

/// Restore the terminal when SIGTERM arrives, then exit 143 (#10476).
///
/// Why: `Drop` does not run on a default SIGTERM, so `kill -TERM` would leave
/// the shell in capture mode, printing escapes on every mouse move.
/// What: registers once per process, on the first capture. The handler stays
/// for the process's life, so the home screen behind the projects view also
/// restores and exits. Outside a tokio runtime, or if registration fails, the
/// default action stays. A no-mouse run never registers, so its signal
/// behaviour is unchanged.
/// Test: `kill -TERM` is a live check; a unit test cannot signal its own runner.
fn install_term_restore() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        static INSTALLED: AtomicBool = AtomicBool::new(false);
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        if INSTALLED.swap(true, Ordering::SeqCst) {
            return;
        }
        let Ok(mut term) = signal(SignalKind::terminate()) else {
            return;
        };
        runtime.spawn(async move {
            if term.recv().await.is_some() {
                restore();
                std::process::exit(143);
            }
        });
    }
}

/// [`enter`] with its two terminal side effects injected, so failure is testable.
///
/// Why: crossterm's raw-mode calls act on the process's real controlling
/// terminal, which a unit test can neither fake nor safely disturb. Taking the
/// screen writer, the raw-mode enable, and the restore as parameters lets a test
/// hand in a writer that fails on `EnterAlternateScreen` and then assert the
/// restore actually ran — the property that was missing (#7224).
/// What: enables raw mode, enters the alternate screen on `screen` (#10476:
/// plus mouse capture when `mouse` is set), and builds the backend over it. On
/// a failure at any step AFTER raw mode engaged it calls `restore_terminal` and
/// propagates the error; a failure of `enable` itself changed nothing, so
/// nothing is restored. The last-step restore goes through `restore_terminal`
/// rather than `screen` because `Terminal::new` consumes the writer on the
/// error path.
/// Test: `enter_with_restores_the_terminal_when_the_screen_fails`,
/// `enter_with_restores_the_terminal_when_capture_fails`,
/// `enter_with_leaves_the_terminal_alone_when_raw_mode_fails`.
pub(super) fn enter_with<W: io::Write>(
    mut screen: W,
    enable: impl FnOnce() -> io::Result<()>,
    restore_terminal: impl FnOnce(),
    mouse: bool,
) -> io::Result<Terminal<CrosstermBackend<W>>> {
    enable()?;
    if let Err(e) = write_enter(&mut screen, mouse) {
        restore_terminal();
        return Err(e);
    }
    Terminal::new(CrosstermBackend::new(screen)).inspect_err(|_| restore_terminal())
}

/// Hand the real screen back while keeping the terminal value alive (#7224).
///
/// Why: the `tm ls` TUI opens a session by shelling out to `tmux attach`, which
/// needs a cooked terminal on the MAIN screen — and control comes back here
/// when the operator detaches, at which point the TUI has to reappear. A
/// `Drop`-only teardown cannot express that: it ends the terminal for good.
/// What: `disable_raw_mode`, then `write_restore` through the terminal's own
/// backend, so the buffered ratatui state stays intact and [`resume`] can pick
/// it back up. #10476: a child process gets a terminal with capture off.
/// Errors propagate: unlike the `Drop` path, this one has a caller that can
/// report them.
/// Test: the sequence by `suspend_disables_and_resume_reenables_only_when_asked`;
/// the hand-off is verified by the tmux smoke run.
pub fn suspend(terminal: &mut TuiTerminal) -> io::Result<()> {
    disable_raw_mode()?;
    write_restore(terminal.backend_mut())
}

/// Take the screen back after a [`suspend`] (#7224).
///
/// Why: the operator detached from the tmux session the TUI handed them to, so
/// control is back here and the list has to reappear where it was.
/// What: re-enters raw mode and the alternate screen through the SAME backend
/// (#10476: and capture, only if this process had it on), then clears ratatui's
/// cached buffer — without the clear, the first frame diffs against what the
/// pre-suspend screen held and paints nothing.
/// Test: the sequence by `suspend_disables_and_resume_reenables_only_when_asked`;
/// verified live by the tmux smoke run.
pub fn resume(terminal: &mut TuiTerminal) -> io::Result<()> {
    enable_raw_mode()?;
    write_enter(
        terminal.backend_mut(),
        CAPTURE_WANTED.load(Ordering::SeqCst),
    )?;
    terminal.clear()
}
