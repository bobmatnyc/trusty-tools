//! `tm fleet init`'s guard-lift step: the owner's choice, off by default (#8436 PR C).
//!
//! Why: ruling Dca A keeps the lift and makes it opt-in. A lift loosens the
//! pm-guard, so every doubt resolves to not lifted (plan §4).
//! What: [`Request`] is what the command line asked; [`step`] turns it and the
//! file's [`LiftState`] into one [`Step`]. `--guard-lift` sets the key,
//! `--no-guard-lift` removes it, and with neither a terminal run asks
//! `[y/N]` while any other run leaves the guard as it is. A lift that was asked
//! for and could not be applied, or a removal that failed, is
//! [`Step::Failed`]; an existing lift with no flag is reported, never removed.
//! Test: `tests/tm_fleet.rs` (`init_with_guard_lift_sets_the_key_and_keeps_other_keys`,
//! `an_unusable_settings_file_fails_the_lift_and_is_left_untouched`,
//! `an_existing_lift_is_kept_without_a_flag_and_removed_only_on_request`),
//! `the_prompt_lifts_only_on_yes`.

use std::io::{BufRead, IsTerminal, Write};
use std::path::Path;

use trusty_mpm::core::architect_guard_lift::{self as lift, LIFT_KEY, LiftChange, LiftState};

use super::Step;

/// The guard-lift choice `tm fleet init` was given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Request {
    /// `--guard-lift`.
    Lift,
    /// `--no-guard-lift`.
    NoLift,
    /// Neither flag: ask on a terminal, else leave the guard as it is.
    Ask,
    /// Neither flag and never ask (the unit tests).
    Unset,
}

impl Request {
    /// The request the two clap flags spell; clap refuses both together.
    pub(crate) fn from_flags(guard_lift: bool, no_guard_lift: bool) -> Self {
        match (guard_lift, no_guard_lift) {
            (true, _) => Self::Lift,
            (_, true) => Self::NoLift,
            _ => Self::Ask,
        }
    }
}

/// Ask `[y/N]` when stdin and stdout are terminals; anything but y/yes is No.
fn ask_on_terminal() -> bool {
    if !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal()) {
        return false;
    }
    print!(
        "Lift the PM guard for the Architect ({LIFT_KEY}=1 in .claude/settings.json)? It lets \
         the Architect act on other sessions' panes directly. [y/N] "
    );
    let _ = std::io::stdout().flush();
    answer_is_yes(std::io::stdin().lock())
}

/// Whether the first line `input` yields is `y` or `yes`; EOF or an error is No.
///
/// Test: `the_prompt_lifts_only_on_yes`.
pub(crate) fn answer_is_yes(mut input: impl BufRead) -> bool {
    let mut line = String::new();
    match input.read_line(&mut line) {
        Ok(n) if n > 0 => matches!(line.trim().to_lowercase().as_str(), "y" | "yes"),
        _ => false,
    }
}

/// The guard-lift step for `dir`; see the module doc.
pub(crate) fn step(dir: &Path, request: Request) -> Step {
    step_with(dir, request, ask_on_terminal)
}

/// [`step`] with the prompt supplied.
pub(crate) fn step_with(dir: &Path, request: Request, ask: impl FnOnce() -> bool) -> Step {
    let state = lift::read_state(dir);
    match (request, &state) {
        (Request::Lift, _) => apply_lift(dir),
        (Request::NoLift, LiftState::Off | LiftState::Other(_)) => {
            Step::Unchanged(format!("guard lift: {}", state.describe()))
        }
        (Request::NoLift, _) => match lift::remove(dir) {
            Ok(LiftChange::Unchanged) => Step::Unchanged("guard lift: off".to_owned()),
            Ok(LiftChange::Written { .. }) => Step::Changed(format!(
                "guard lift: removed {LIFT_KEY} (the PM guard is on)"
            )),
            Err(why) => Step::Failed(format!(
                "guard lift: could not be removed, so the lift may still be on: {why}"
            )),
        },
        (_, LiftState::Lifted) => Step::Unchanged(
            "guard lift: lifted (existing; `tm fleet init --no-guard-lift` removes it)".to_owned(),
        ),
        (_, LiftState::Unknown(_)) => Step::Skipped(format!("guard lift: {}", state.describe())),
        (Request::Ask, _) if ask() => apply_lift(dir),
        (_, LiftState::Other(_)) => Step::Unchanged(format!("guard lift: {}", state.describe())),
        (_, LiftState::Off) => Step::Unchanged("guard lift: off (no choice given)".to_owned()),
    }
}

/// Set the key; a failure is [`Step::Failed`] and leaves the file as it was.
fn apply_lift(dir: &Path) -> Step {
    match lift::set(dir) {
        Ok(LiftChange::Unchanged) => {
            Step::Unchanged("guard lift: lifted (owner choice at init)".to_owned())
        }
        Ok(LiftChange::Written { was }) => Step::Changed(format!(
            "guard lift: set {LIFT_KEY}=1 in .claude/settings.json (owner choice){}",
            was.map(|v| format!("; was {v}")).unwrap_or_default()
        )),
        Err(why) => Step::Failed(format!("guard lift: not applied: {why}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prompt_lifts_only_on_yes() {
        for (input, want) in [
            ("y\n", true),
            ("YES\n", true),
            (" yes \n", true),
            ("n\n", false),
            ("\n", false),
            ("", false),
            ("yeah\n", false),
        ] {
            assert_eq!(answer_is_yes(input.as_bytes()), want, "{input:?}");
        }
        let dir = tempfile::tempdir().unwrap();
        let declined = step_with(dir.path(), Request::Ask, || false);
        assert_eq!(
            declined,
            Step::Unchanged("guard lift: off (no choice given)".to_owned())
        );
        assert!(!lift::settings_path(dir.path()).exists());
        let unset = step_with(dir.path(), Request::Unset, || panic!("asked"));
        assert_eq!(unset, declined);
        assert!(matches!(
            step_with(dir.path(), Request::Ask, || true),
            Step::Changed(_)
        ));
        assert_eq!(lift::read_state(dir.path()), LiftState::Lifted);
    }
}
