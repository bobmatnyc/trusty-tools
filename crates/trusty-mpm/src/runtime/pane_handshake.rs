//! Prove a pane's shell is at a PRIMARY prompt before a launch is typed at it
//! (#8233 review round 2, findings 1 and 8).
//!
//! Why: `managed_launch::deliver` used to send `C-c` and then immediately type
//! the launch line, with nothing between the two confirming the shell had
//! actually returned to a prompt. Two live failures came out of that gap. A pane
//! whose `.zshrc` was still running (`direnv` stalling on `gh auth token` during
//! the launch `cd`) took the keystrokes into a canonical-mode tty that nothing
//! was reading. A pane sitting at a `quote>` continuation prompt swallowed three
//! successive launch commands as literal text inside one open quote, because
//! `C-c` alone is a hope, not evidence.
//!
//! What: [`confirm_prompt`] types a SHORT fixed-shape probe and waits to see the
//! probe's OUTPUT appear in the pane. Output can only appear once the shell
//! parsed and executed the line, so a pane still in its init hooks, or holding
//! an unterminated construct, produces nothing and the probe times out. On a
//! timeout the recovery is an interrupt — never a closing quote, never a bare
//! Enter, either of which would RUN whatever the open construct had accumulated
//! — and the probe is repeated, so the flush is confirmed rather than assumed.
//!
//! [`continuation_kind`] classifies a captured pane tail so the two failures are
//! reported as DISTINCT states: "wedged at a continuation prompt" and "no prompt
//! yet" call for the same recovery here but are different bugs to an operator,
//! and the launch verifier names them apart.
//!
//! The probe is deliberately not a length check on the launch line. The typed
//! line cannot be partially delivered by construction — it names a launch spec
//! file and is bounded by `core::tmux::MAX_PANE_COMMAND_BYTES`, pinned by
//! `pane_line_is_short_for_the_worst_case_launch` — so there is nothing to
//! echo-scrape. This handshake answers the other half: whether anything is
//! READING the tty at all.
//!
//! Test: `crates/trusty-mpm/src/runtime/pane_handshake_tests.rs`.

use std::time::Duration;

use crate::session_manager::ManagedTmuxDriver;

/// Probe rounds [`confirm_prompt`] runs before giving up.
///
/// Why: round one asks a pane that is probably fine; an interrupt and round two
/// covers the wedged case. A third round has nothing new to try — if an
/// interrupt plus a full probe window did not produce a prompt, the pane is not
/// one a launch should be typed into.
pub(crate) const PROMPT_PROBE_ROUNDS: u32 = 2;

/// Capture polls per round. Paired with [`PROMPT_PROBE_INTERVAL`] this is a
/// 1.5 s window per round, 3 s worst case, and it returns the moment the
/// probe's output appears — typically inside 200 ms.
///
/// Why bounded so tightly: [`confirm_prompt`] is reached from the SYNCHRONOUS
/// `RuntimeAdapter::spawn`, so its waiting is a blocking sleep on the caller's
/// thread. The budget only has to cover a shell finishing its init hooks, which
/// is the case this issue's captures show; a runtime that is merely slow to
/// START is covered much later and much more generously by
/// `daemon::managed_routes::launch_verify::verify_launch`.
pub(crate) const PROMPT_PROBE_ATTEMPTS: u32 = 15;

/// Delay between [`confirm_prompt`] captures. See [`PROMPT_PROBE_ATTEMPTS`].
pub(crate) const PROMPT_PROBE_INTERVAL: Duration = Duration::from_millis(100);

/// Lines of pane tail [`confirm_prompt`] reads. The probe's output is the last
/// thing printed, so a short tail is enough and keeps the capture cheap.
const PROBE_CAPTURE_LINES: usize = 12;

/// Secondary-prompt renderings that mean "the parser is holding an open
/// construct" (#8233, live evidence).
///
/// Why: these are zsh's `PS2` forms — the observed failures were `quote>` (an
/// unterminated single quote, session `tm-apex-companion`) and `cmdand cursh>`
/// (an unterminated `{` group, session `tm-writing-01`). Recognising them is
/// what separates "wedged" from "not started yet" in the operator-facing
/// report; the RECOVERY for both is the same interrupt.
/// What: matched as a suffix of the pane's last non-empty line, so a composed
/// form like `cmdand cursh>` matches on its final token.
/// Test: `continuation_kind_recognises_an_open_quote`,
/// `continuation_kind_recognises_a_composed_zsh_prompt`,
/// `continuation_kind_ignores_an_ordinary_prompt`.
const CONTINUATION_PROMPTS: &[&str] = &[
    "quote>",
    "dquote>",
    "bquote>",
    "cursh>",
    "cmdsubst>",
    "braceparam>",
    "brace>",
    "math>",
    "subst>",
    "array>",
    "heredoc>",
    "heredocx>",
    "for>",
    "while>",
    "until>",
    "repeat>",
    "if>",
    "then>",
    "else>",
    "elif>",
    "fi>",
    "do>",
    "done>",
    "case>",
    "esac>",
    "select>",
    "func>",
    "pipe>",
    "and>",
    "or>",
];

/// What [`confirm_prompt`] concluded about the pane's shell.
///
/// Why: the caller needs three decisions out of one call — proceed, abandon and
/// say the shell was wedged, abandon and say nothing was reading the tty — plus
/// a fourth answer for a driver that cannot see the pane at all, which must
/// change nothing (every hermetic test and the documented tmux-absent fallback
/// live there).
/// What: only [`Self::Continuation`] and [`Self::Unresponsive`] are failures.
/// Test: one test per variant in `pane_handshake_tests.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PaneState {
    /// The probe's output came back: the shell is executing what it is typed.
    Ready,
    /// No probe output, and the pane tail shows a shell continuation prompt —
    /// the parser is holding an open construct. Carries the marker seen.
    Continuation(String),
    /// No probe output and no continuation prompt: nothing is reading the tty.
    Unresponsive,
    /// This driver cannot capture the pane, so nothing was learned and nothing
    /// was waited for.
    Unobservable,
}

impl PaneState {
    /// May a launch be typed into a pane in this state?
    ///
    /// What: `true` for [`Self::Ready`] and [`Self::Unobservable`] — the second
    /// because "cannot tell" must never become a new refusal (the false-positive
    /// that got an earlier `runtime_ready` gate reverted, see
    /// `session_manager::task_inject::PaneReadiness`).
    /// Test: `unobservable_panes_are_still_launched_into`.
    pub(crate) fn may_launch(&self) -> bool {
        matches!(self, PaneState::Ready | PaneState::Unobservable)
    }

    /// The operator-facing reason a launch was refused, or `None` when it was
    /// not.
    ///
    /// Test: `a_wedged_pane_names_the_continuation_marker`,
    /// `an_unresponsive_pane_says_nothing_was_reading_the_tty`.
    pub(crate) fn refusal(&self) -> Option<String> {
        match self {
            PaneState::Ready | PaneState::Unobservable => None,
            PaneState::Continuation(marker) => Some(format!(
                "the pane's shell is wedged at a '{marker}' continuation prompt and did not \
                 return to a primary prompt after an interrupt — it is holding an unterminated \
                 construct from an earlier command and would have swallowed this launch as more \
                 of it. Nothing was typed and nothing was started (#8233)"
            )),
            PaneState::Unresponsive => Some(
                "the pane's shell never executed a probe command, so nothing is reading its \
                 tty — typically a shell still running its init hooks (a slow `direnv` on the \
                 launch directory). Nothing was typed and nothing was started (#8233)"
                    .to_owned(),
            ),
        }
    }
}

/// Classify a captured pane tail as a shell continuation prompt.
///
/// Why: see [`CONTINUATION_PROMPTS`]. Pure and separate from the I/O so the
/// recognition table can be tested against real captured text.
/// What: takes the last NON-EMPTY line, trims it, and returns the
/// [`CONTINUATION_PROMPTS`] entry it ends with. A line ending in a bare `>` is
/// deliberately NOT matched — that is bash's default `PS2`, and it is also how
/// plenty of ordinary prompts end, so matching it would refuse healthy launches.
/// Test: `continuation_kind_recognises_an_open_quote`,
/// `continuation_kind_recognises_a_composed_zsh_prompt`,
/// `continuation_kind_ignores_an_ordinary_prompt`,
/// `continuation_kind_ignores_empty_output`.
pub(crate) fn continuation_kind(captured: &str) -> Option<&'static str> {
    let last = captured.lines().rev().find(|l| !l.trim().is_empty())?;
    let last = last.trim_end();
    CONTINUATION_PROMPTS
        .iter()
        .find(|marker| last.ends_with(*marker))
        .copied()
}

/// The text a probe prints when the shell RUNS it, for launch `nonce`.
///
/// Why: the needle must not appear in the pane merely because the probe was
/// typed. `echo` is present in every POSIX shell and prints nothing else.
/// Test: `the_probe_line_does_not_contain_its_own_output`.
fn probe_output(nonce: &str) -> String {
    format!("tm-ready-{nonce}")
}

/// The literal prefix of every probe line. See [`probe_line`].
const PROBE_PREFIX: &str = "echo tm-rea\"dy\"-";

/// The probe command line typed at the prompt.
///
/// Why: the quotes split the needle so the TYPED text and the PRINTED text
/// differ — `echo tm-rea"dy"-<nonce>` on screen, `tm-ready-<nonce>` in the
/// output. Scraping for the output alone therefore cannot be satisfied by the
/// echo of the keystrokes, which is the whole point: only execution counts.
/// What: about 30 bytes, far under `core::tmux::MAX_PANE_COMMAND_BYTES`, so the
/// probe is never itself a victim of the truncation it guards against.
/// Test: `the_probe_line_does_not_contain_its_own_output`.
fn probe_line(nonce: &str) -> String {
    format!("{PROBE_PREFIX}{nonce}")
}

/// What a shell would PRINT for `text`, when `text` is a probe line.
///
/// Why: this is the seam a hermetic [`ManagedTmuxDriver`] double answers the
/// handshake through. Without it every fake driver would be a shell that reads
/// nothing, [`confirm_prompt`] would report [`PaneState::Unresponsive`], and the
/// whole hermetic test corpus would start refusing launches for a reason that
/// exists only in the test double. Keeping the recognition beside the
/// construction is what stops the two drifting.
/// What: `Some(<the printed line>)` for a probe line, `None` for anything else —
/// so a double can also tell a probe apart from the launch line it is actually
/// asserting on.
/// Test: `probe_reply_answers_only_a_probe_line`.
#[cfg(test)]
pub(crate) fn probe_reply(text: &str) -> Option<String> {
    text.strip_prefix(PROBE_PREFIX).map(probe_output)
}

/// Read the pane's tail, pane-scoped when a pane id is known.
///
/// What: `None` when the driver cannot capture — the [`PaneState::Unobservable`]
/// signal, never an assertion that the pane is bad.
fn capture_tail(
    tmux: &dyn ManagedTmuxDriver,
    tmux_name: &str,
    pane_id: Option<&str>,
) -> Option<String> {
    match pane_id {
        Some(pane) => tmux.capture_pane(tmux_name, pane, PROBE_CAPTURE_LINES).ok(),
        None => tmux.capture(tmux_name, PROBE_CAPTURE_LINES).ok(),
    }
}

/// Flush whatever the shell's parser is holding.
///
/// Why: an interrupt DISCARDS the open construct. A closing quote or a bare
/// Enter would instead RUN whatever had accumulated — on this issue's live
/// pane that was three concatenated launch commands inside one open quote.
/// What: pane-scoped when known (sibling-window hijack, #2456). Best-effort:
/// the trait default for both methods is an error, so a driver with no
/// interrupt support simply skips it and the next probe reports the result.
fn flush_pane(tmux: &dyn ManagedTmuxDriver, tmux_name: &str, pane_id: Option<&str>) {
    let sent = match pane_id {
        Some(pane) => tmux.send_interrupt_to_pane(tmux_name, pane),
        None => tmux.send_interrupt(tmux_name),
    };
    if let Err(e) = sent {
        tracing::debug!(
            session = %tmux_name,
            "could not interrupt the pane before probing it (non-fatal, expected for drivers \
             with no interrupt support, #8233): {e}"
        );
    }
}

/// Wait `interval` without parking a Tokio worker (#8233 round 3, finding 5).
///
/// Why: every `RuntimeAdapter` entry point is synchronous, but four async call
/// sites (`lifecycle::resume_managed`, `launch_on_main`, `auto_relaunch`) reach
/// this from the runtime that also serves the daemon's HTTP routes. A bare
/// `std::thread::sleep` of up to 3 s there stalls whatever else that worker was
/// going to run — the discipline `launch_verify::verify_launch` documents.
/// What: on a multi-thread runtime, `block_in_place` hands this task's worker
/// back to the scheduler for the duration; anywhere else (a current-thread
/// runtime, or no runtime at all — the CLI's own launch path) it is a plain
/// sleep, because `block_in_place` panics outside a multi-thread runtime.
/// `spawn_blocking` was the review's suggestion and is rejected here: it needs
/// every argument `Send + 'static`, so it would clone the driver `Arc` and the
/// spec at four call sites to move a wait that `block_in_place` moves in place —
/// and the #8233 claim `resume_managed` holds is a local guard in that future,
/// which `block_in_place` leaves exactly where it is.
/// Test: `the_handshake_does_not_park_a_tokio_worker`.
fn park(interval: Duration) {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(|| std::thread::sleep(interval));
        }
        _ => std::thread::sleep(interval),
    }
}

/// Prove the pane's shell is executing what it is typed, recovering once.
///
/// Why: see this module's header — this is the CONFIRMED prompt that finding 1
/// says the launcher was typing without.
/// What: captures the tail once to decide observability (a driver that cannot
/// capture answers [`PaneState::Unobservable`] immediately and pays no delay).
/// Then, for up to `rounds`: interrupt the pane when a continuation prompt is
/// visible or a previous round failed, type [`probe_line`], and poll the tail
/// `attempts` times for [`probe_output`]. First sighting is
/// [`PaneState::Ready`]. Exhaustion reports [`PaneState::Continuation`] when a
/// continuation prompt was seen in any round, else [`PaneState::Unresponsive`].
/// A probe the driver refuses to send is [`PaneState::Unresponsive`] at once —
/// a pane that cannot be typed into cannot be launched into either.
///
/// Waits through [`park`], which never stalls a Tokio worker;
/// `attempts`/`interval` are parameters so tests spend none of the production
/// budget.
/// Test: `confirm_prompt_is_ready_when_the_probe_echoes`,
/// `an_unobservable_session_is_not_called_unresponsive`,
/// `the_handshake_does_not_park_a_tokio_worker`,
/// `confirm_prompt_reports_a_wedged_continuation_prompt`,
/// `confirm_prompt_reports_an_unresponsive_shell`,
/// `confirm_prompt_interrupts_a_continuation_prompt_before_probing`,
/// `confirm_prompt_recovers_a_pane_that_frees_up_after_the_interrupt`,
/// `confirm_prompt_is_unobservable_when_the_driver_cannot_capture`,
/// `confirm_prompt_is_unobservable_when_the_existence_probe_errors`,
/// `confirm_prompt_never_sends_a_closing_quote_or_a_bare_enter`.
pub(crate) fn confirm_prompt(
    tmux: &dyn ManagedTmuxDriver,
    tmux_name: &str,
    pane_id: Option<&str>,
    rounds: u32,
    attempts: u32,
    interval: Duration,
) -> PaneState {
    // #8233 round 3, finding 4: ask observability FIRST, exactly as
    // `launch_verify::verify_launch` does. A driver that cannot see the session
    // answers `Ok("")` to a capture — the documented tmux-absent fallback and
    // every hermetic double — and an empty tail is indistinguishable from a
    // shell that read nothing, so the launch was refused as `Unresponsive`
    // after the full blocking budget. "Cannot tell" must never become a
    // refusal; this module's own rule at `PaneState::may_launch`.
    // #8233 r7: `session_exists` maps a driver ERROR to `false`, which is the
    // same answer as "confirmed absent". Both arms land on `Unobservable`
    // anyway, so the tri-state probe is used to keep the error VISIBLE — a
    // swallowed probe failure that silently skipped this handshake and typed
    // the launch line into a wedged pane is the defect this module exists to
    // close, and a warn line is what makes it diagnosable after the fact.
    match tmux.session_exists_checked(tmux_name) {
        Ok(true) => {}
        Ok(false) => return PaneState::Unobservable,
        Err(e) => {
            // Deliberately PERMISSIVE: an unobservable probe must never become
            // a launch refusal (this module's rule at `PaneState::may_launch`),
            // so the launch proceeds unverified — logged, never silent.
            tracing::warn!(
                session = %tmux_name,
                "pane handshake: tmux could not answer whether this session exists; \
                 proceeding without the wedged-pane check (#8233): {e}"
            );
            return PaneState::Unobservable;
        }
    }
    let Some(first) = capture_tail(tmux, tmux_name, pane_id) else {
        return PaneState::Unobservable;
    };
    let mut wedged = continuation_kind(&first).map(str::to_owned);
    for round in 0..rounds.max(1) {
        // #8233: flush before the FIRST probe too when the pane is visibly
        // wedged — probing into an open construct only lengthens it.
        if round > 0 || wedged.is_some() {
            flush_pane(tmux, tmux_name, pane_id);
        }
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        let needle = probe_output(&nonce);
        if tmux
            .send_command_line(tmux_name, pane_id, &probe_line(&nonce))
            .is_err()
        {
            return PaneState::Unresponsive;
        }
        for _ in 0..attempts.max(1) {
            park(interval);
            let Some(tail) = capture_tail(tmux, tmux_name, pane_id) else {
                return PaneState::Unobservable;
            };
            if tail.contains(&needle) {
                return PaneState::Ready;
            }
            if wedged.is_none() {
                wedged = continuation_kind(&tail).map(str::to_owned);
            }
        }
    }
    match wedged {
        Some(marker) => PaneState::Continuation(marker),
        None => PaneState::Unresponsive,
    }
}

#[cfg(test)]
#[path = "pane_handshake_tests.rs"]
mod tests;
