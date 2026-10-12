//! The input-box state contract and the send path that enforces it.

use std::fmt;
use std::str::FromStr;

use crate::error::SendError;
use crate::tmux::{PaneId, Tmux, send_keys};

/// What a Claude Code pane's input box (the last `❯` line) holds.
///
/// Why: Claude Code draws an auto-generated next-prompt suggestion in dim text,
/// which a plain capture shows exactly like a human's draft. The Architect must
/// never submit or overwrite a draft.
/// What: the four states `input-state.py` reports, under the same names
/// (`no-prompt`, `empty`, `suggestion`, `typed`). Classifying a capture into a
/// state is not ported yet (#9171 A2).
/// Test: `input_state_names_round_trip`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InputState {
    /// No `❯` line on screen: not at a prompt.
    NoPrompt,
    /// The box is empty.
    Empty,
    /// The box holds only a dim auto-suggestion, which typing replaces.
    Suggestion,
    /// The box holds text someone typed.
    Typed,
}

impl InputState {
    /// Every state, in the order `input-state.py` documents them.
    pub const ALL: [Self; 4] = [Self::NoPrompt, Self::Empty, Self::Suggestion, Self::Typed];

    /// The name `input-state.py` prints for this state.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoPrompt => "no-prompt",
            Self::Empty => "empty",
            Self::Suggestion => "suggestion",
            Self::Typed => "typed",
        }
    }

    /// True when keys may be sent: the box is `empty` or holds a `suggestion`.
    ///
    /// Same rule as `fleet-poll.py`'s `box not in ("empty", "suggestion")`.
    pub fn accepts_input(self) -> bool {
        matches!(self, Self::Empty | Self::Suggestion)
    }
}

impl fmt::Display for InputState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A name that is none of the four [`InputState`] names.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown input-box state {0:?}")]
pub struct UnknownInputState(pub String);

impl FromStr for InputState {
    type Err = UnknownInputState;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|s| s.as_str() == name)
            .ok_or_else(|| UnknownInputState(name.to_owned()))
    }
}

/// Sends `keys` to `pane` only if its input box accepts input.
///
/// Why: a send into a box holding a draft submits or corrupts the draft. The
/// check belongs on the one path that sends, so no caller can skip it.
/// What: when `observed.accepts_input()` is false, returns
/// [`SendError::Refused`] and makes no tmux call; otherwise runs one
/// `send-keys -t <pane> -- <keys…>`. `observed` must be read from the pane just
/// before the call.
/// Test: `send_guarded_refuses_typed_and_sends_on_empty`; the doctest below
/// proves the unguarded `send_keys` is not part of the public API.
///
/// ```compile_fail,E0599
/// use trusty_architect::{FakeTmux, PaneId, Tmux};
/// let pane = PaneId::parse("%1").unwrap();
/// let _ = FakeTmux::new().send_keys(&pane, &["x"]);
/// ```
pub fn send_guarded<T: Tmux + ?Sized>(
    tmux: &T,
    pane: &PaneId,
    observed: InputState,
    keys: &[&str],
) -> Result<(), SendError> {
    if !observed.accepts_input() {
        return Err(SendError::Refused {
            pane: pane.to_string(),
            state: observed,
        });
    }
    send_keys(tmux, pane, keys)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FakeTmux;

    #[test]
    fn input_state_names_round_trip() {
        for state in InputState::ALL {
            assert_eq!(state.as_str().parse::<InputState>(), Ok(state));
            assert_eq!(state.to_string(), state.as_str());
        }
        assert_eq!(
            "Typed".parse::<InputState>(),
            Err(UnknownInputState("Typed".to_owned()))
        );
    }

    #[test]
    fn send_guarded_refuses_typed_and_sends_on_empty() -> Result<(), crate::TmuxError> {
        let pane = PaneId::parse("%261")?;
        let sent = [["send-keys", "-t", "%261", "--", "/tm-fleet-check", "Enter"]];
        let cases = [
            (InputState::Typed, false),
            (InputState::NoPrompt, false),
            (InputState::Empty, true),
            (InputState::Suggestion, true),
        ];
        for (state, sends) in cases {
            let fake = FakeTmux::new();
            let result = send_guarded(&fake, &pane, state, &["/tm-fleet-check", "Enter"]);
            if sends {
                assert!(result.is_ok(), "{state}: {result:?}");
                assert_eq!(fake.calls(), sent, "{state} must send exactly once");
            } else {
                assert!(
                    matches!(
                        result,
                        Err(SendError::Refused { ref pane, state: s }) if pane == "%261" && s == state
                    ),
                    "{state} must be refused, got {result:?}"
                );
                assert!(fake.calls().is_empty(), "{state} must send nothing");
            }
        }
        Ok(())
    }
}
