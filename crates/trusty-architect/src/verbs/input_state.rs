//! `tm architect input-state`: the port of `input-state.py`'s command line.
//!
//! Why: the Architect's skills classify a pane's input box before every send
//! with `input-state.py '=<session>:'` and read its one-line answer (#9822).
//! What: [`input_state`] captures each target with the script's argv
//! (`capture-pane -t <target> -p -e`), classifies it with
//! [`crate::classify_input`], and prints the script's line:
//! `<target> <state> | <text, first 100 characters>`, `<target> no-prompt |`,
//! or `<target> missing |` when the capture fails. The script exits 0 after a
//! failed capture; the verb prints the same line and then fails.
//! [`InputTarget`] accepts only an exact target: a `%N` pane id or an
//! `=`-prefixed session target.
//! Test: `input_state_cli_matches_python` (tests/verbs_parity.rs),
//! `input_target_accepts_only_exact_targets`,
//! `a_failed_capture_prints_missing_and_fails`.

use super::{Output, VerbError};
use crate::input::{InputState, classify_input};
use crate::tmux::{PaneId, Tmux};

/// The characters of input text the script prints (`text[:100]`).
const TEXT_MAX: usize = 100;

/// An exact tmux target: a `%N` pane id or `=<session>[:<window>]`.
///
/// Why: tmux resolves a bare name by prefix, so `tm-a` can hit `tm-ab`
/// (#8443). The script passes its argument through unchecked; the verb
/// refuses a target that is not exact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputTarget(String);

impl InputTarget {
    /// Parses `%N` or `=<name>…`.
    ///
    /// # Errors
    /// [`VerbError::Usage`] for any other text.
    pub fn parse(value: &str) -> Result<Self, VerbError> {
        let exact_session = value.len() > 1 && value.starts_with('=');
        if exact_session || PaneId::parse(value).is_ok() {
            Ok(Self(value.to_owned()))
        } else {
            Err(VerbError::Usage(format!(
                "not an exact tmux target: {value:?} (want %N or =<session>:)"
            )))
        }
    }

    /// The `-t` target, exactly as given.
    pub fn as_target(&self) -> &str {
        &self.0
    }
}

/// Classifies each target's input box; see the module doc.
pub fn input_state<T: Tmux + ?Sized>(tmux: &T, targets: &[InputTarget]) -> Output {
    let mut stdout = String::new();
    let mut failed = Vec::new();
    for target in targets {
        let t = target.as_target();
        match tmux.run(&["capture-pane", "-t", target.as_target(), "-p", "-e"]) {
            Err(e) => {
                stdout.push_str(&format!("{t} missing |\n"));
                failed.push(format!("{t}: {e}"));
            }
            Ok(capture) => match classify_input(&capture) {
                (InputState::NoPrompt, _) => stdout.push_str(&format!("{t} no-prompt |\n")),
                (state, text) => {
                    let shown: String = text.chars().take(TEXT_MAX).collect();
                    stdout.push_str(&format!("{t} {} | {shown}\n", state.as_str()));
                }
            },
        }
    }
    let error = (!failed.is_empty())
        .then(|| VerbError::Failed(format!("cannot capture {}", failed.join("; "))));
    Output { stdout, error }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ScriptedTmux;

    #[test]
    fn input_target_accepts_only_exact_targets() {
        for ok in ["%0", "%261", "=tm-a:", "=tm-a", "=tm-a:0.1"] {
            assert!(InputTarget::parse(ok).is_ok(), "{ok}");
        }
        for bad in ["tm-a", "=", "%", "%x", "", "tm-a:0", "@1"] {
            let err = InputTarget::parse(bad).expect_err(bad);
            assert_eq!(err.exit_code(), 2, "{bad}");
        }
    }

    #[test]
    fn a_failed_capture_prints_missing_and_fails() -> Result<(), VerbError> {
        let tmux = ScriptedTmux::new()
            .script("capture-pane", "\u{276f}\u{a0}hi\n")
            .fail("capture-pane", "can't find pane: %9");
        let targets = [InputTarget::parse("%1")?, InputTarget::parse("%9")?];
        let out = input_state(&tmux, &targets);
        assert_eq!(out.stdout, "%1 typed | hi\n%9 missing |\n");
        let err = out.error.expect("a failed capture must fail the verb");
        assert_eq!(err.exit_code(), 1);
        assert!(err.to_string().contains("can't find pane"), "{err}");
        Ok(())
    }
}
