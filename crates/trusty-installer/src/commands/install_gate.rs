//! Pre-install confirmation gate — the #2112 default-safe posture for `tctl install`.
//!
//! Why: #2112 — during the 2026-07-06 deploy, `tctl install trusty-mpm` was run
//! from a non-interactive (non-TTY) context expecting it to no-op/abort without
//! an explicit `--yes`. Instead it silently proceeded and reactivated a dormant
//! launchd service. The pre-existing confirmation prompt was gated on
//! `is_tty()`, so any piped, scripted, or agent-driven invocation skipped
//! confirmation entirely and installed for real. This mirrors the #1316 safety
//! property already enforced for `tctl upgrade` via
//! [`super::update_engine::decide_apply`]: encoding the decision as one pure,
//! exhaustively-testable function makes the gate auditable independent of any
//! terminal or process side effect.
//!
//! What: [`decide_install_gate`] takes whether `--yes` was passed and whether
//! stdin/stdout is a TTY, and returns an [`InstallGate`] verdict: `Proceed`
//! (yes was given — install without prompting), `NeedsPrompt` (a TTY is
//! available — the caller shows the interactive blast-radius prompt), or
//! `Refuse` (non-TTY and no `--yes` — cannot obtain consent, so `tctl install`
//! must hard-fail rather than silently install). `--dry-run` short-circuits
//! before this gate is ever consulted (see `install::run`) since a preview
//! never needs consent.
//!
//! Test: `tests::decide_yes_proceeds`, `tests::decide_tty_needs_prompt`,
//! `tests::decide_non_tty_refuses`.

/// Inputs to the pure pre-install confirmation gate.
///
/// Why: A single-argument-struct keeps [`decide_install_gate`] trivial to call
/// and exhaustively test across the 2×2 decision matrix.
/// What: `yes` — the global `--yes`/`-y` flag; `is_tty` — whether stdin is an
/// interactive terminal we could prompt on.
/// Test: `tests::decide_*` construct these inline.
#[derive(Clone, Copy, Debug)]
pub struct GateInputs {
    /// The `--yes` flag (explicit non-interactive consent).
    pub yes: bool,
    /// Whether stdin is an interactive terminal we can prompt on.
    pub is_tty: bool,
}

/// The verdict of the pre-install confirmation gate.
///
/// Why: Separates the *decision* from prompting/printing so `install::run` can
/// be tested without a real terminal.
/// What: `Proceed` — install without prompting (explicit `--yes`); `NeedsPrompt`
/// — a TTY is available, show the interactive blast-radius confirmation;
/// `Refuse` — no TTY and no `--yes`; consent cannot be obtained, so the install
/// must hard-fail (#2112's core fix) rather than silently proceed.
/// Test: `tests::decide_*`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallGate {
    /// Explicit consent already given — install without prompting.
    Proceed,
    /// A TTY is available — the caller must show the interactive prompt.
    NeedsPrompt,
    /// No TTY and no `--yes` — refuse to install (cannot obtain consent).
    Refuse,
}

/// The pure pre-install confirmation gate (#2112 default-safe posture).
///
/// Why: THE safety property #2112 asks for: `tctl install` must never mutate
/// the system (real binaries, launchd services) without either explicit
/// consent (`--yes`) or an interactive confirmation the operator can actually
/// see and answer (a TTY). Encoding the rule as one pure function makes it
/// auditable and exhaustively testable, decoupled from the terminal.
///
/// What:
/// 1. `--yes` → [`InstallGate::Proceed`] (explicit non-interactive consent).
/// 2. Not `--yes`, on a TTY → [`InstallGate::NeedsPrompt`] (caller prompts).
/// 3. Not `--yes`, not a TTY → [`InstallGate::Refuse`] (consent impossible;
///    the caller must hard-fail with a message pointing at `--yes`/`--dry-run`
///    rather than silently installing — this is the exact gap #2112 reported).
///
/// Test: `tests::decide_yes_proceeds`, `tests::decide_tty_needs_prompt`,
/// `tests::decide_non_tty_refuses`.
pub fn decide_install_gate(inputs: GateInputs) -> InstallGate {
    if inputs.yes {
        InstallGate::Proceed
    } else if inputs.is_tty {
        InstallGate::NeedsPrompt
    } else {
        InstallGate::Refuse
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: `--yes` must always short-circuit to `Proceed`, TTY or not — it is
    /// the documented automation escape hatch (DOC-5 §3.3 / §4.3).
    /// What: Both TTY states with `yes: true` yield `Proceed`.
    /// Test: This is the test.
    #[test]
    fn decide_yes_proceeds() {
        for is_tty in [true, false] {
            assert_eq!(
                decide_install_gate(GateInputs { yes: true, is_tty }),
                InstallGate::Proceed
            );
        }
    }

    /// Why: On a TTY without `--yes`, the operator can see and answer a
    /// prompt — the gate must hand control back to the caller, not refuse.
    /// What: `yes: false, is_tty: true` yields `NeedsPrompt`.
    /// Test: This is the test.
    #[test]
    fn decide_tty_needs_prompt() {
        assert_eq!(
            decide_install_gate(GateInputs {
                yes: false,
                is_tty: true
            }),
            InstallGate::NeedsPrompt
        );
    }

    /// Why: THIS is the #2112 regression case — a piped/scripted/agent-driven
    /// invocation (non-TTY) with no `--yes` must never silently install.
    /// What: `yes: false, is_tty: false` yields `Refuse`.
    /// Test: This is the test.
    #[test]
    fn decide_non_tty_refuses() {
        assert_eq!(
            decide_install_gate(GateInputs {
                yes: false,
                is_tty: false
            }),
            InstallGate::Refuse
        );
    }
}
