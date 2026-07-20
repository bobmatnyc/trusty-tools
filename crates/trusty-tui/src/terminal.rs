//! The panic-safe terminal lifecycle: [`TerminalGuard`].
//!
//! Why: tagent's current `run_tui` (`crates/trusty-agents/src/repl/tui/run.rs`
//! `setup_terminal`/`restore_terminal`) only restores the terminal AFTER
//! `event_loop()` returns normally — a panic anywhere inside the loop unwinds
//! straight past that teardown call and leaves the operator's terminal in raw
//! mode + the alternate screen (no echo, no visible prompt, `Ctrl-C` doesn't
//! even show up). That is a real bug in the code this crate will eventually
//! replace, not a spec this crate should reproduce. `trusty-mpm`'s coordinator
//! TUI (`crates/trusty-mpm/src/tui/coordinator/mod.rs` `TerminalGuard`) already
//! fixed this the right way — restoration lives in `Drop`, which Rust runs on
//! BOTH the normal return path and the panic-unwind path — and this module
//! generalizes that fix to be the one terminal-lifecycle type both tagent
//! (Slice 10) and tcode (Slice 3) share.
//!
//! What: [`TerminalGuard::enter`] performs the three terminal-mutating steps
//! (raw mode, alternate screen, mouse capture) and returns a guard alongside
//! a ready-to-use `ratatui::Terminal`. The guard's `Drop` best-effort undoes
//! all three; every step swallows its own error since a `Drop` impl cannot
//! propagate one and a partial restore is still strictly better than none.
//! The actual OS-level restore calls are routed through the private
//! [`TerminalOps`] trait so [`tests`] can substitute a recording fake instead
//! of exercising a real TTY — sandboxes and CI routinely have no controlling
//! terminal at all, and the property under test ("does `Drop` run during
//! unwind") has nothing to do with whether `disable_raw_mode()` itself would
//! succeed on this particular host.
//!
//! # Spec References
//! - [`SPEC-TTUI-05~draft`](../../../docs/specs/DOC-50-tcode-tui-claude-code-clone.md#SPEC-TTUI-05~draft) — Slice 2 deliverable (§5, Slice 2): panic-safe terminal guard.

use std::io::{self, Stdout};

use crossterm::{
    cursor::Show,
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};

/// The terminal-restoring side effects [`TerminalGuard::drop`] performs.
///
/// Why: see the module doc comment — this indirection exists purely so
/// [`tests`] can verify the unwind-safety *contract* (`Drop` runs) without
/// depending on a real TTY being present. Production code only ever sees
/// [`CrosstermOps`].
trait TerminalOps: Send {
    fn leave(&mut self);
}

/// The real implementation: disable raw mode, leave the alternate screen,
/// disable mouse capture, and show the cursor again. Every step is
/// best-effort (see the module doc comment for why errors are swallowed).
struct CrosstermOps;

impl TerminalOps for CrosstermOps {
    fn leave(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            LeaveAlternateScreen,
            DisableMouseCapture,
            Show
        );
    }
}

/// RAII guard that restores the terminal on drop — including during a panic.
///
/// Why: see the module doc comment. Holding this guard for the lifetime of
/// the render loop is what makes terminal restoration unconditional: there is
/// no code path (early return, `?`, or panic) that can skip `Drop`.
/// What: constructed by [`TerminalGuard::enter`], which is the ONLY way to
/// get one — that constructor is also what performs the matching "enter" side
/// effects, so a `TerminalGuard` existing is always proof the terminal was
/// actually put into raw/alt-screen/mouse-capture mode. `Drop` undoes all
/// three, ignoring errors (nothing useful can be done while unwinding).
/// Test: [`tests::drop_restores_on_normal_scope_exit`],
/// [`tests::drop_restores_on_panic_unwind`].
pub struct TerminalGuard {
    ops: Box<dyn TerminalOps>,
}

impl TerminalGuard {
    /// Enter raw mode, the alternate screen, and mouse capture, returning a
    /// guard (whose `Drop` restores all three) alongside a ready-to-`draw`
    /// `ratatui::Terminal`.
    ///
    /// Why: bundling guard + terminal construction into one fallible call
    /// means a caller can never end up with a live `Terminal` and no guard
    /// (or vice versa) — the two are only ever created together.
    /// What: `enable_raw_mode()` first; if entering the alternate screen or
    /// mouse capture then fails, raw mode is unwound before the error is
    /// returned (so a failed `enter()` never leaves the terminal half-mutated
    /// for the caller to clean up). On success, builds a
    /// `CrosstermBackend<Stdout>`-backed `Terminal`.
    /// Test: exercised transitively by [`crate::run`]'s event-loop tests via
    /// `ratatui::backend::TestBackend`; `enter()` itself needs a real TTY so
    /// it is not unit-tested here (mirrors the precedent in
    /// `crates/trusty-agents/src/repl/tui/run.rs::setup_terminal`, which is
    /// likewise exercised only by launching the TUI).
    pub fn enter() -> io::Result<(Self, Terminal<CrosstermBackend<Stdout>>)> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(e) = execute!(stdout, EnterAlternateScreen, EnableMouseCapture) {
            // Best-effort unwind of the step that DID succeed before
            // propagating — otherwise a failed `enter()` leaves raw mode on
            // with no guard in the caller's hands to undo it.
            let _ = disable_raw_mode();
            return Err(e);
        }
        let backend = CrosstermBackend::new(stdout);
        let terminal = match Terminal::new(backend) {
            Ok(t) => t,
            Err(e) => {
                let _ = disable_raw_mode();
                let _ = execute!(
                    io::stdout(),
                    LeaveAlternateScreen,
                    DisableMouseCapture,
                    Show
                );
                return Err(e);
            }
        };
        Ok((
            Self {
                ops: Box::new(CrosstermOps),
            },
            terminal,
        ))
    }

    /// Test-only constructor: build a guard around a recording [`TerminalOps`]
    /// fake instead of performing real terminal I/O.
    #[cfg(test)]
    fn with_ops(ops: Box<dyn TerminalOps>) -> Self {
        Self { ops }
    }
}

impl Drop for TerminalGuard {
    /// Why: see [`TerminalGuard`] — runs on both normal return and panic
    /// unwind, which is the entire point of this type existing.
    /// What: delegates to the guard's [`TerminalOps::leave`].
    /// Test: [`tests::drop_restores_on_normal_scope_exit`],
    /// [`tests::drop_restores_on_panic_unwind`].
    fn drop(&mut self) {
        self.ops.leave();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic::{self, AssertUnwindSafe};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Records how many times `leave()` was called instead of touching a
    /// real TTY — see the module doc comment for why.
    struct RecordingOps(Arc<AtomicUsize>);

    impl TerminalOps for RecordingOps {
        fn leave(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// The ordinary case: a guard going out of scope normally must restore
    /// exactly once.
    #[test]
    fn drop_restores_on_normal_scope_exit() {
        let count = Arc::new(AtomicUsize::new(0));
        {
            let _guard = TerminalGuard::with_ops(Box::new(RecordingOps(count.clone())));
            assert_eq!(
                count.load(Ordering::SeqCst),
                0,
                "must not restore on construction"
            );
        }
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    /// The bug this type exists to fix: tagent's current `run_tui` only
    /// restores the terminal AFTER the event loop returns, so a panic mid-loop
    /// unwinds past that call and leaves the terminal in raw/alt-screen mode.
    /// This test proves `TerminalGuard` closes that gap — a panic while the
    /// guard is held still runs `Drop`, because unwinding runs destructors for
    /// every value on the stack, not just values reached by a normal return.
    #[test]
    fn drop_restores_on_panic_unwind() {
        let count = Arc::new(AtomicUsize::new(0));
        let count_in_closure = count.clone();
        let result = panic::catch_unwind(AssertUnwindSafe(|| {
            let _guard = TerminalGuard::with_ops(Box::new(RecordingOps(count_in_closure)));
            panic!("simulated panic mid-render-loop");
        }));
        assert!(
            result.is_err(),
            "the panic must have propagated to catch_unwind"
        );
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "TerminalGuard::drop must run during unwind, restoring the terminal exactly once"
        );
    }

    /// A guard dropped early (e.g. an explicit `drop(guard)` before a longer
    /// scope ends) must not restore a second time when the scope itself later
    /// ends — `Drop` runs exactly once per value, never twice.
    #[test]
    fn drop_restores_exactly_once_even_when_dropped_early() {
        let count = Arc::new(AtomicUsize::new(0));
        let guard = TerminalGuard::with_ops(Box::new(RecordingOps(count.clone())));
        drop(guard);
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }
}
