//! The input-box state contract and the send path that enforces it.

use std::fmt;
use std::str::FromStr;

use crate::classify::matchers::strip_sgr;
use crate::error::SendError;
use crate::pystr::{splitlines, strip};
use crate::tmux::{Keys, PaneId, Tmux, send_keys};

/// What a Claude Code pane's input box (the last `❯` line) holds.
///
/// Why: Claude Code draws an auto-generated next-prompt suggestion in dim text,
/// which a plain capture shows exactly like a human's draft. The Architect must
/// never submit or overwrite a draft.
/// What: the four states `input-state.py` reports, under the same names
/// (`no-prompt`, `empty`, `suggestion`, `typed`). [`classify_input`] is the
/// parity port of the script's classifier; [`crate::input_state_for_send`] is
/// the fail-closed reading `send_guarded` takes (#9171 A2).
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

/// `input-state.py`'s `classify`: the state and text of the last `❯` row.
///
/// Why: the A2 parity port of the prototype classifier (#9171), kept exact so
/// `parse_pane`'s `box` and `box_text` fields match the Python byte for byte.
/// What: no `❯` row is `NoPrompt`; otherwise the text after the first `❯` on
/// the last such row, SGR escapes removed and stripped, is `Empty` when blank,
/// `Suggestion` when the raw row carries dim (`ESC[2m`), else `Typed`. It reads
/// one row only and does not check that the row is the input box; use
/// [`crate::input_state_for_send`] before sending keys.
/// Test: `test_input_box_classifier_is_input_states`,
/// `input_state_matches_python_goldens`.
pub fn classify_input(capture: &str) -> (InputState, String) {
    let Some(last) = splitlines(capture)
        .into_iter()
        .rev()
        .find(|l| l.contains('❯'))
    else {
        return (InputState::NoPrompt, String::new());
    };
    let raw = last.split_once('❯').map_or("", |(_, r)| r);
    let text = strip(&strip_sgr(raw)).to_owned();
    let state = if text.is_empty() {
        InputState::Empty
    } else if raw.contains("\u{1b}[2m") {
        InputState::Suggestion
    } else {
        InputState::Typed
    };
    (state, text)
}

/// An input-box reading taken by [`crate::input_state_for_send`], the only
/// reading [`send_guarded`] accepts.
///
/// Why: `send_guarded` took a bare [`InputState`], so a caller could hand it
/// [`classify_input`]'s reading, which reads a stale `❯` above a busy turn as
/// `empty` (#9171 A3a reconcile list, PR #9799).
/// What: wraps the state in a private field; outside this crate's tests the
/// only constructor is [`crate::input_state_for_send`].
/// Test: `send_guarded_refuses_typed_and_sends_on_empty`; the doctest on
/// [`send_guarded`] proves `classify_input`'s state does not type-check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SendReading(InputState);

impl SendReading {
    pub(crate) fn new(state: InputState) -> Self {
        Self(state)
    }

    /// A reading built from `state` directly, for this crate's unit tests.
    #[cfg(test)]
    pub(crate) fn for_test(state: InputState) -> Self {
        Self(state)
    }

    /// The input-box state this reading carries.
    pub fn state(self) -> InputState {
        self.0
    }

    /// True when keys may be sent: the box is `empty` or holds a `suggestion`.
    pub fn accepts_input(self) -> bool {
        self.0.accepts_input()
    }
}

impl PartialEq<InputState> for SendReading {
    fn eq(&self, other: &InputState) -> bool {
        self.0 == *other
    }
}

impl fmt::Display for SendReading {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Sends `keys` to `pane` only if its input box accepts input.
///
/// Why: a send into a box holding a draft submits or corrupts the draft. The
/// check belongs on the one path that sends, so no caller can skip it.
/// What: when `observed.accepts_input()` is false, returns
/// [`SendError::Refused`] and makes no tmux call; otherwise runs one
/// `send-keys` ([`Keys::Literal`] types text, [`Keys::Named`] presses keys).
/// `observed` must be read from the pane just before the call.
/// Test: `send_guarded_refuses_typed_and_sends_on_empty`; the doctests below
/// prove the unguarded `send_keys` is not public and that `classify_input`'s
/// reading is not accepted.
///
/// ```compile_fail,E0599
/// use trusty_architect::{FakeTmux, PaneId, Tmux};
/// let pane = PaneId::parse("%1").unwrap();
/// let _ = FakeTmux::new().send_keys(&pane, &["x"]);
/// ```
///
/// ```compile_fail,E0308
/// use trusty_architect::{FakeTmux, Keys, PaneId, classify_input, send_guarded};
/// let pane = PaneId::parse("%1").unwrap();
/// let (state, _) = classify_input("❯ ");
/// let _ = send_guarded(&FakeTmux::new(), &pane, state, Keys::Literal("x"));
/// ```
pub fn send_guarded<T: Tmux + ?Sized>(
    tmux: &T,
    pane: &PaneId,
    observed: SendReading,
    keys: Keys<'_>,
) -> Result<(), SendError> {
    if !observed.accepts_input() {
        return Err(SendError::Refused {
            pane: pane.to_string(),
            state: observed.state(),
        });
    }
    send_keys(tmux, pane, keys)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FakeTmux, NamedKey};

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
        let sent = [
            vec!["send-keys", "-t", "%261", "-l", "--", "/tm-fleet-check"],
            vec!["send-keys", "-t", "%261", "Enter"],
        ];
        let cases = [
            (InputState::Typed, false),
            (InputState::NoPrompt, false),
            (InputState::Empty, true),
            (InputState::Suggestion, true),
        ];
        for (state, sends) in cases {
            let fake = FakeTmux::new();
            let reading = SendReading::for_test(state);
            let typed = send_guarded(&fake, &pane, reading, Keys::Literal("/tm-fleet-check"));
            let entered = send_guarded(&fake, &pane, reading, Keys::Named(&[NamedKey::Enter]));
            if sends {
                assert!(typed.is_ok() && entered.is_ok(), "{state}: {typed:?} {entered:?}");
                assert_eq!(fake.calls(), sent, "{state} must send exactly once each");
            } else {
                assert!(
                    matches!(
                        typed,
                        Err(SendError::Refused { ref pane, state: s }) if pane == "%261" && s == state
                    ),
                    "{state} must be refused, got {typed:?}"
                );
                assert!(fake.calls().is_empty(), "{state} must send nothing");
            }
        }
        Ok(())
    }
}
