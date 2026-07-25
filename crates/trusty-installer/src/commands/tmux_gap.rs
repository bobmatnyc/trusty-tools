//! Decide what `tctl install` does when trusty-mpm is selected and tmux is
//! still missing after the prereq-check phase (#3821).
//!
//! Why: On a truly clean macOS box (no Homebrew, no tmux) the piped
//! `curl | sh -s -- -y` demo install had no way to auto-install tmux — Phase
//! 6's `install_hint_for` correctly returns `auto_cmd: None` when no package
//! manager is detected (there is nothing to safely run non-interactively:
//! bootstrapping Homebrew itself needs sudo/CLT and can prompt) — but the
//! caller (`install::run`) treated "still missing" as a soft warning and
//! continued installing the rest of the stable set anyway, only to fail at
//! the very end (exit 2, from the post-install verify tail finding
//! trusty-mpm's session management unhealthy) with no single actionable
//! message pointing at the real cause. Encoding the decision as one pure,
//! exhaustively-testable function (mirroring [`super::install_gate`]) lets
//! `install::run` fail EARLY — before any install side effect — with one
//! clear message, instead of a confusing half-installed dead end.
//!
//! What: [`decide_tmux_gap_action`] takes whether trusty-mpm was selected,
//! whether tmux is still missing after Phase 6, `--yes`, `--json`, and
//! whether stdin is a TTY, and returns a [`TmuxGapAction`] verdict:
//! - `None` — nothing to do (tmux present, or trusty-mpm not selected).
//! - `FailEarly` — `--yes` was given (the piped/scripted/agent-driven path)
//!   and tmux could not be made present; abort BEFORE installing anything
//!   else, in both human and `--json` modes.
//! - `PromptContinue` — an interactive TTY, no `--yes`: ask the operator
//!   whether to continue anyway (they can see the warning and decide).
//! - `WarnAndContinue` — non-interactive, no `--yes`, human output: print an
//!   informational warning. In practice `install::run`'s later
//!   `decide_install_gate` refuses this exact combination anyway (no
//!   consent obtainable), so this is best-effort context for the log, not a
//!   load-bearing gate.
//!
//! Test: `tests::decide_*` exhaustively covers the decision matrix.

/// Inputs to the pure tmux-gap decision.
///
/// Why: A single-argument-struct keeps [`decide_tmux_gap_action`] trivial to
/// call and exhaustively test.
///
/// What: `mpm_selected` — whether `trusty-mpm` is among the members being
/// installed; `tmux_still_missing` — whether tmux remained absent after
/// Phase 6's detect/auto-install/prompt attempt; `yes`/`json`/`is_tty` mirror
/// the same global flags [`super::install_gate::GateInputs`] uses.
///
/// Test: `tests::decide_*` construct these inline.
#[derive(Clone, Copy, Debug)]
pub struct TmuxGapInputs {
    /// Whether `trusty-mpm` is among the selected members.
    pub mpm_selected: bool,
    /// Whether tmux remained absent after the Phase 6 prereq-check phase.
    pub tmux_still_missing: bool,
    /// The `--yes` flag (explicit non-interactive consent).
    pub yes: bool,
    /// Whether `--json` mode is active.
    pub json: bool,
    /// Whether stdin is an interactive terminal we could prompt on.
    pub is_tty: bool,
}

/// The verdict of the tmux-gap decision.
///
/// Why: Separates the *decision* from prompting/printing/exiting so
/// `install::run` can be tested without a real terminal or process exit.
///
/// What: See the module doc for the meaning of each variant.
///
/// Test: `tests::decide_*`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TmuxGapAction {
    /// Nothing to do.
    None,
    /// `--yes` given, tmux still missing: abort before installing anything
    /// else, with one clear message (human and `--json`).
    FailEarly,
    /// Interactive TTY, no `--yes`: ask whether to continue anyway.
    PromptContinue,
    /// Non-interactive, no `--yes`, human output: print an informational
    /// warning (the install_gate refuses this combination immediately
    /// afterward regardless).
    WarnAndContinue,
}

/// The pure tmux-gap decision (#3821 fail-early fix).
///
/// Why: THE property #3821 asks for: a non-interactive (`--yes`) install
/// must never silently limp forward once it is clear trusty-mpm's tmux
/// requirement cannot be met, only to report a confusing generic failure
/// after installing everything else. Encoding the rule as one pure function
/// makes it auditable and testable independent of any process side effect.
///
/// What:
/// 1. `!mpm_selected || !tmux_still_missing` → [`TmuxGapAction::None`] (no
///    gap to act on).
/// 2. `yes` → [`TmuxGapAction::FailEarly`] (json or not — the caller shapes
///    the message per output mode, but always aborts before installing).
/// 3. `!json && is_tty` → [`TmuxGapAction::PromptContinue`].
/// 4. `!json` (remaining case: no TTY, no `--yes`) →
///    [`TmuxGapAction::WarnAndContinue`].
/// 5. `json` (remaining case) → [`TmuxGapAction::None`] (machine output
///    stays silent here; `decide_install_gate` refuses this combination
///    immediately afterward).
///
/// Test: `tests::decide_yes_fails_early`, `tests::decide_tty_prompts`,
/// `tests::decide_non_tty_warns`, `tests::decide_json_no_yes_is_silent`,
/// `tests::decide_no_gap_when_present_or_unselected`.
pub fn decide_tmux_gap_action(inputs: TmuxGapInputs) -> TmuxGapAction {
    if !inputs.mpm_selected || !inputs.tmux_still_missing {
        return TmuxGapAction::None;
    }
    if inputs.yes {
        return TmuxGapAction::FailEarly;
    }
    if !inputs.json && inputs.is_tty {
        return TmuxGapAction::PromptContinue;
    }
    if !inputs.json {
        return TmuxGapAction::WarnAndContinue;
    }
    TmuxGapAction::None
}

/// Build the `--json` error payload for a [`TmuxGapAction::FailEarly`].
///
/// Why (#3821 finding 2, code-critic MEDIUM on PR #3879): when a package
/// manager IS present but the auto-install attempt itself fails
/// (network/disk/permissions), the static `hint` alone ("Homebrew
/// required...") is misleading — it reads as "no brew found" even though
/// brew ran and failed for some other reason. The captured error
/// (`Missing.detail`) must reach `--json` consumers as a field DISTINCT from
/// `hint`, never silently dropped.
///
/// What: `hint` is always the static, platform-appropriate manual note;
/// `detail` is `Some(captured stderr)` only when an auto-install attempt was
/// actually made and failed, `None` when no attempt was possible at all
/// (e.g. no package manager detected) — serialised as JSON `null` in that
/// case, never conflated with `hint`.
///
/// Test: `tests::fail_early_json_no_package_manager_has_null_detail`,
/// `tests::fail_early_json_attempt_failed_carries_detail`.
pub fn fail_early_json_payload(error: &str, hint: &str, detail: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "command": "install",
        "error": error,
        "hint": hint,
        "detail": detail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> TmuxGapInputs {
        TmuxGapInputs {
            mpm_selected: true,
            tmux_still_missing: true,
            yes: false,
            json: false,
            is_tty: false,
        }
    }

    /// Why: `--yes` (the piped `curl | sh -s -- -y` demo path, #3821's exact
    /// repro) must always fail early, TTY or not, json or not — this is the
    /// core fix.
    /// What: `yes: true` across all json/is_tty combinations yields
    /// `FailEarly`.
    /// Test: This is the test.
    #[test]
    fn decide_yes_fails_early() {
        for json in [true, false] {
            for is_tty in [true, false] {
                let inputs = TmuxGapInputs {
                    yes: true,
                    json,
                    is_tty,
                    ..base()
                };
                assert_eq!(
                    decide_tmux_gap_action(inputs),
                    TmuxGapAction::FailEarly,
                    "json={json} is_tty={is_tty}"
                );
            }
        }
    }

    /// Why: an interactive operator without `--yes` can see and answer a
    /// prompt — offer to continue anyway rather than aborting unconditionally.
    /// What: `yes: false, json: false, is_tty: true` yields `PromptContinue`.
    /// Test: This is the test.
    #[test]
    fn decide_tty_prompts() {
        let inputs = TmuxGapInputs {
            is_tty: true,
            ..base()
        };
        assert_eq!(
            decide_tmux_gap_action(inputs),
            TmuxGapAction::PromptContinue
        );
    }

    /// Why: no TTY and no `--yes` (human output) still deserves a warning in
    /// the log even though `decide_install_gate` refuses the overall install
    /// moments later — best-effort context, never a hard gate here.
    /// What: `yes: false, json: false, is_tty: false` yields
    /// `WarnAndContinue`.
    /// Test: This is the test.
    #[test]
    fn decide_non_tty_warns() {
        assert_eq!(
            decide_tmux_gap_action(base()),
            TmuxGapAction::WarnAndContinue
        );
    }

    /// Why: `--json` without `--yes` must stay silent here — a human warning
    /// line has no place in machine-readable output, and
    /// `decide_install_gate` refuses this combination regardless.
    /// What: `yes: false, json: true` (either `is_tty`) yields `None`.
    /// Test: This is the test.
    #[test]
    fn decide_json_no_yes_is_silent() {
        for is_tty in [true, false] {
            let inputs = TmuxGapInputs {
                json: true,
                is_tty,
                ..base()
            };
            assert_eq!(decide_tmux_gap_action(inputs), TmuxGapAction::None);
        }
    }

    /// Why: no gap exists when tmux is present, or when trusty-mpm was never
    /// selected — the action must be a no-op even under `--yes`.
    /// What: `tmux_still_missing: false` and `mpm_selected: false` (each with
    /// `yes: true`) both yield `None`.
    /// Test: This is the test.
    #[test]
    fn decide_no_gap_when_present_or_unselected() {
        let tmux_present = TmuxGapInputs {
            tmux_still_missing: false,
            yes: true,
            ..base()
        };
        assert_eq!(decide_tmux_gap_action(tmux_present), TmuxGapAction::None);

        let mpm_not_selected = TmuxGapInputs {
            mpm_selected: false,
            yes: true,
            ..base()
        };
        assert_eq!(
            decide_tmux_gap_action(mpm_not_selected),
            TmuxGapAction::None
        );
    }

    /// Why (#3821 finding 2): the no-package-manager case must carry a
    /// `null` `detail` — never a fabricated or reused-`hint` value — so a
    /// `--json` consumer can tell "no attempt was possible" apart from "an
    /// attempt was made and failed".
    /// What: `detail: None` in; asserts `hint` is set and `detail` is JSON
    /// `null`.
    /// Test: This is the test.
    #[test]
    fn fail_early_json_no_package_manager_has_null_detail() {
        let payload = fail_early_json_payload(
            "tmux is required for trusty-mpm",
            "Install tmux: `brew install tmux` (Homebrew required — https://brew.sh)",
            None,
        );
        assert_eq!(
            payload["hint"],
            "Install tmux: `brew install tmux` (Homebrew required — https://brew.sh)"
        );
        assert!(
            payload["detail"].is_null(),
            "expected null detail, got: {payload}"
        );
    }

    /// Why (#3821 finding 2 — the core fix): when an auto-install attempt
    /// WAS made and failed, the real captured error must reach `--json`
    /// output as a field distinct from the static `hint`, never dropped.
    /// What: `detail: Some(captured stderr)` in; asserts `detail` carries it
    /// verbatim and is NOT equal to `hint`.
    /// Test: This is the test.
    #[test]
    fn fail_early_json_attempt_failed_carries_detail() {
        let payload = fail_early_json_payload(
            "tmux is required for trusty-mpm",
            "Install tmux: `brew install tmux` (Homebrew required — https://brew.sh)",
            Some("brew install tmux: Permission denied @ dir_s_mkdir - /usr/local/Cellar"),
        );
        assert_eq!(
            payload["detail"],
            "brew install tmux: Permission denied @ dir_s_mkdir - /usr/local/Cellar"
        );
        assert_ne!(
            payload["detail"], payload["hint"],
            "detail must never be conflated with the static hint"
        );
    }
}
