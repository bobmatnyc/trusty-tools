//! The guard-lift-at-init floor of `tm hook --pm-guard` (#8436 PR C, M1).
//!
//! Why: ruling Dca A makes lifting the Architect's PM guard the owner's choice
//! at `tm fleet init`. A session that runs `tm fleet init --guard-lift` (or
//! `scripts/init.sh --guard-lift`) in one Bash call would take that choice
//! without writing `.claude/settings.json` itself, so the lift needs its own
//! deny: the flag is for the owner's terminal, which no hook guards.
//! What: [`evaluate_guard_lift_init`] walks every segment of a Bash command
//! (through `sh -c` / `env` / `xargs` wrappers, via [`super::floor_d4`]) and
//! denies one that names a `fleet … init` or an `init.sh` together with
//! `--guard-lift` or `--guard-lift=…`, in any order, and any
//! `ARCHITECT_GUARD_LIFT=<not 0>` assignment, which `init.sh` reads as the
//! same request. Every caller is bound, the Architect included, under every
//! bypass. `--no-guard-lift` is a different token and passes.
//! FAIL-CLOSED: a segment that does not lex but names the lift denies.
//! Residual: a script file or shell function that hides the flag from the
//! command text is not seen (the same residual as the D4 floor).
//! Test: `guard_lift_init_tests.rs`, `pm_guard_floor_tests.rs`
//! (`a_guard_lift_at_init_is_denied_to_every_hook_guarded_session`).

use super::segments;

/// The rule name recorded with a guard-lift-at-init deny.
pub(crate) const GUARD_LIFT_INIT_RULE: &str = "guard-lift-init";

/// The lift flag `tm fleet init` and `init.sh` take.
const FLAG: &str = "--guard-lift";

/// The env var `init.sh` reads as the same request.
const ENV: &str = "ARCHITECT_GUARD_LIFT";

/// The deny for a command that asks to lift the Architect's PM guard, or `None`.
///
/// Test: `a_guard_lift_at_init_is_denied_in_every_spelling`,
/// `an_unparseable_command_naming_the_lift_is_denied`, `everything_else_passes`.
pub(crate) fn evaluate_guard_lift_init(command: &str) -> Option<String> {
    // Cheap exit: every deny names one of these words.
    if !command.contains("guard-lift") && !command.contains(ENV) {
        return None;
    }
    for seg in segments(command) {
        let Some(argv) = shlex::split(seg.text.trim()) else {
            if seg.text.contains("guard-lift") || seg.text.contains(ENV) {
                return Some(reason());
            }
            continue;
        };
        if asks_for_the_lift(&argv) {
            return Some(reason());
        }
    }
    None
}

/// Whether one segment's argv asks for the lift.
fn asks_for_the_lift(argv: &[String]) -> bool {
    let flag = argv
        .iter()
        .any(|t| t == FLAG || t.starts_with(&format!("{FLAG}=")));
    let env = argv.iter().any(|t| {
        t.strip_prefix(&format!("{ENV}="))
            .is_some_and(|v| !v.is_empty() && v != "0")
    });
    let init_sh = argv.iter().any(|t| t.rsplit('/').next() == Some("init.sh"));
    let fleet_init = argv
        .iter()
        .position(|t| t == "fleet")
        .is_some_and(|i| argv[i + 1..].iter().any(|t| t == "init"));
    env || (flag && (init_sh || fleet_init))
}

/// The deny text.
fn reason() -> String {
    format!(
        "Hard-floor deny (#8436): `{FLAG}` (or `{ENV}=1`) lifts the Architect's PM guard, which \
         is the owner's choice at `tm fleet init` / `scripts/init.sh`, made from their own \
         terminal. A hook-guarded session cannot take it. Ask the owner to run it."
    )
}

#[cfg(test)]
#[path = "guard_lift_init_tests.rs"]
mod tests;
