//! The wake pointer's send sequence as typed steps (#9171 A3b-1).
//!
//! Why: [`crate::send_guarded`] checks a [`SendReading`] before every send,
//! but once the poller has typed its pointer the box reads `typed`, so the
//! same guard would refuse the Enter that submits it and the BSpace that
//! removes it (A3 plan section 0, findings 4-5). The prototype answers that by
//! sending them unguarded, confirming with a substring test that can submit a
//! human draft merged with the pointer. Here each send needs the value the
//! step before it produced, so the order cannot be skipped and Enter has one
//! way in: an equality check.
//! What: [`verify_owner`] reads `#{session_name}` and returns an [`Owned`]
//! only for `tm-architect`. [`type_pointer`] consumes an `Owned` and a
//! `SendReading` that accepts input, types the message, and returns
//! [`Typed`]. [`Typed::confirm`] compares the normalised box text with the
//! normalised message: equal is [`Confirmed`], anything else [`Mismatch`].
//! [`Confirmed::submit`] consumes a fresh `Owned` and presses Enter.
//! [`Mismatch::erase`] consumes a fresh `Owned` and presses BSpace once per
//! character this call typed, only when the box ends with that text;
//! otherwise it sends nothing. No type here is `Clone`, and the only
//! constructors are these functions, so each send costs its own owner check.
//! Architect ruling Q6: everything here is crate-private.
//! Test: `submit_needs_equality`, `erase_only_our_own_tail`,
//! `owner_is_checked_per_send`, `type_pointer_refuses_a_reading_or_empty_text`.

use crate::error::WakeError;
use crate::input::SendReading;
use crate::poller::box_text::norm;
use crate::pystr::strip;
use crate::tmux::{Keys, NamedKey, PaneId, Tmux, send_keys};

/// Proof that `pane` belonged to `tm-architect` when it was last read.
///
/// Not `Clone`: every send consumes one, so every send re-reads the owner.
#[derive(Debug)]
pub(crate) struct Owned {
    pane: PaneId,
}

/// Reads the pane's live session and returns [`Owned`] only when it is
/// exactly `owner` (the prototype's `send_to_supervisor` check).
///
/// `owner` is the config's supervisor session on every production path
/// ([`crate::poller::Waker::dispatch_as`]; [`crate::poller::SUPERVISOR`] unless
/// `ARCHITECT_SESSION` names another, #9822);
/// only the scratch-server live test passes its own throwaway session.
///
/// # Errors
/// [`WakeError::NotOwner`] for any other session; [`WakeError::Tmux`] when
/// `display-message` fails.
pub(crate) fn verify_owner<T: Tmux + ?Sized>(
    tmux: &T,
    pane: &PaneId,
    owner: &str,
) -> Result<Owned, WakeError> {
    let out = tmux.run(&[
        "display-message",
        "-p",
        "-t",
        pane.as_target(),
        "#{session_name}",
    ])?;
    let session = strip(&out);
    if session != owner {
        return Err(WakeError::NotOwner {
            pane: pane.to_string(),
            session: session.to_owned(),
        });
    }
    Ok(Owned { pane: pane.clone() })
}

/// Fails unless `owned` was read for `pane`.
fn same_pane(owned: &Owned, pane: &PaneId) -> Result<(), WakeError> {
    if owned.pane == *pane {
        return Ok(());
    }
    Err(WakeError::WrongPane {
        checked: owned.pane.to_string(),
        pane: pane.to_string(),
    })
}

/// The pointer is in the box; what the box holds is not yet known.
#[derive(Debug)]
pub(crate) struct Typed {
    pane: PaneId,
    msg_norm: String,
    typed_chars: usize,
}

/// Types `msg` into the owned pane, once, if `reading` accepts input.
///
/// `reading` moves in, so one reading authorises one typing.
///
/// # Errors
/// [`WakeError::Refused`] when the reading is not `empty`/`suggestion`;
/// [`WakeError::EmptyMessage`] when `msg` is blank (a blank text would make
/// every box "end with" it); [`WakeError::Tmux`] when `send-keys` fails.
pub(crate) fn type_pointer<T: Tmux + ?Sized>(
    tmux: &T,
    owned: Owned,
    reading: SendReading,
    msg: &str,
) -> Result<Typed, WakeError> {
    let pane = owned.pane;
    if !reading.accepts_input() {
        return Err(WakeError::Refused {
            pane: pane.to_string(),
            state: reading.state(),
        });
    }
    let msg_norm = norm(msg);
    if msg_norm.is_empty() {
        return Err(WakeError::EmptyMessage(pane.to_string()));
    }
    send_keys(tmux, &pane, Keys::Literal(msg))?;
    Ok(Typed {
        pane,
        msg_norm,
        typed_chars: msg.chars().count(),
    })
}

/// The result of comparing the box with what was typed.
#[derive(Debug)]
pub(crate) enum Confirmation {
    /// The box holds exactly the pointer.
    Confirmed(Confirmed),
    /// The box holds anything else.
    Mismatch(Mismatch),
}

impl Typed {
    /// `Confirmed` only when `box_norm` (normalised box text) equals the
    /// normalised message (#9171: equality, not the prototype's substring).
    pub(crate) fn confirm(self, box_norm: &str) -> Confirmation {
        if box_norm == self.msg_norm {
            Confirmation::Confirmed(Confirmed { pane: self.pane })
        } else {
            Confirmation::Mismatch(Mismatch {
                ours_at_end: box_norm.ends_with(&self.msg_norm),
                pane: self.pane,
                typed_chars: self.typed_chars,
            })
        }
    }
}

/// The box holds exactly the pointer; Enter may submit it.
#[derive(Debug)]
pub(crate) struct Confirmed {
    pane: PaneId,
}

impl Confirmed {
    /// Presses Enter in the confirmed pane.
    ///
    /// # Errors
    /// [`WakeError::WrongPane`] when `owned` was read for another pane;
    /// [`WakeError::Tmux`] when `send-keys` fails.
    pub(crate) fn submit<T: Tmux + ?Sized>(self, tmux: &T, owned: Owned) -> Result<(), WakeError> {
        same_pane(&owned, &self.pane)?;
        send_keys(tmux, &self.pane, Keys::Named(&[NamedKey::Enter]))?;
        Ok(())
    }
}

/// The box holds something other than exactly the pointer.
#[derive(Debug)]
pub(crate) struct Mismatch {
    pane: PaneId,
    ours_at_end: bool,
    typed_chars: usize,
}

/// What [`Mismatch::erase`] did.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Erased {
    /// Pressed BSpace this many times: the characters this call typed.
    Ours(usize),
    /// The box did not end with our text; nothing was sent.
    Left,
}

impl Mismatch {
    /// True when the box ends with the text this call typed, so erasing that
    /// many characters removes only our own text.
    pub(crate) fn ours_at_end(&self) -> bool {
        self.ours_at_end
    }

    /// Removes the text this call typed, and nothing else.
    ///
    /// Sends `BSpace` x the typed character count only when the box ended
    /// with our text; otherwise returns [`Erased::Left`] and sends nothing. The
    /// count comes from the typing step, so it can never exceed what was typed.
    ///
    /// # Errors
    /// [`WakeError::WrongPane`] when `owned` was read for another pane;
    /// [`WakeError::Tmux`] when `send-keys` fails.
    pub(crate) fn erase<T: Tmux + ?Sized>(
        self,
        tmux: &T,
        owned: Owned,
    ) -> Result<Erased, WakeError> {
        same_pane(&owned, &self.pane)?;
        if !self.ours_at_end {
            return Ok(Erased::Left);
        }
        let keys = vec![NamedKey::BSpace; self.typed_chars];
        send_keys(tmux, &self.pane, Keys::Named(&keys))?;
        Ok(Erased::Ours(self.typed_chars))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::InputState;
    use crate::poller::SUPERVISOR;
    use crate::{FakeTmux, ScriptedTmux};

    fn pane(id: &str) -> Result<PaneId, crate::TmuxError> {
        PaneId::parse(id)
    }

    fn owner(tmux: &ScriptedTmux, id: &str) -> Result<Owned, WakeError> {
        verify_owner(tmux, &pane(id)?, SUPERVISOR)
    }

    fn arch() -> ScriptedTmux {
        ScriptedTmux::new()
            .fallback("display-message", "tm-architect\n")
            .fallback("send-keys", "")
    }

    #[test]
    fn submit_needs_equality() -> Result<(), WakeError> {
        let msg = "[poll 08:53Z] 3 new alerts -> /x/alerts.jsonl";
        for (box_text, enter) in [
            ("[poll08:53Z]3newalerts->/x/alerts.jsonl", true),
            ("[poll08:53Z]3newalerts->/x/alerts.jsonlmore", false),
            ("draft[poll08:53Z]3newalerts->/x/alerts.jsonl", false),
            ("", false),
        ] {
            let tmux = arch();
            let typed = type_pointer(
                &tmux,
                owner(&tmux, "%1")?,
                SendReading::for_test(InputState::Empty),
                msg,
            )?;
            match typed.confirm(box_text) {
                Confirmation::Confirmed(c) => {
                    assert!(enter, "{box_text:?} must not confirm");
                    c.submit(&tmux, owner(&tmux, "%1")?)?;
                }
                Confirmation::Mismatch(_) => assert!(!enter, "{box_text:?} must confirm"),
            }
            let enters = tmux.sends().iter().filter(|c| c[3] == "Enter").count();
            assert_eq!(enters, usize::from(enter), "{box_text:?}");
        }
        Ok(())
    }

    #[test]
    fn erase_only_our_own_tail() -> Result<(), WakeError> {
        let msg = "[poll] ï new alerts ->";
        for (box_text, want) in [
            ("draft[poll]ïnewalerts->", Erased::Ours(msg.chars().count())),
            ("[poll]ïnewalerts->draft", Erased::Left),
            ("", Erased::Left),
        ] {
            let tmux = arch();
            let typed = type_pointer(
                &tmux,
                owner(&tmux, "%1")?,
                SendReading::for_test(InputState::Suggestion),
                msg,
            )?;
            let Confirmation::Mismatch(m) = typed.confirm(box_text) else {
                panic!("{box_text:?} confirmed");
            };
            assert_eq!(m.erase(&tmux, owner(&tmux, "%1")?)?, want, "{box_text:?}");
            let bspaces = tmux
                .sends()
                .iter()
                .flatten()
                .filter(|k| *k == "BSpace")
                .count();
            let expect = match want {
                Erased::Ours(n) => n,
                Erased::Left => 0,
            };
            assert_eq!(bspaces, expect);
            assert!(bspaces <= msg.chars().count() && msg.chars().count() < msg.len());
        }
        Ok(())
    }

    #[test]
    fn owner_is_checked_per_send() -> Result<(), WakeError> {
        let tmux = ScriptedTmux::new().fallback("display-message", "tm-api\n");
        assert!(matches!(
            owner(&tmux, "%1"),
            Err(WakeError::NotOwner { .. })
        ));
        let tmux = ScriptedTmux::new().fail("display-message", "no server");
        assert!(matches!(owner(&tmux, "%1"), Err(WakeError::Tmux(_))));
        // An owner check for %2 cannot authorise Enter in %1.
        let tmux = arch();
        let typed = type_pointer(
            &tmux,
            owner(&tmux, "%1")?,
            SendReading::for_test(InputState::Empty),
            "hi",
        )?;
        let Confirmation::Confirmed(c) = typed.confirm("hi") else {
            panic!("hi must confirm");
        };
        assert!(matches!(
            c.submit(&tmux, owner(&tmux, "%2")?),
            Err(WakeError::WrongPane { .. })
        ));
        assert_eq!(tmux.sends().len(), 1, "only the typing went out");
        Ok(())
    }

    #[test]
    fn type_pointer_refuses_a_reading_or_empty_text() -> Result<(), WakeError> {
        let fake = FakeTmux::new();
        let owned = Owned { pane: pane("%1")? };
        let refused = type_pointer(&fake, owned, SendReading::for_test(InputState::Typed), "hi");
        assert!(matches!(
            refused,
            Err(WakeError::Refused {
                state: InputState::Typed,
                ..
            })
        ));
        let owned = Owned { pane: pane("%1")? };
        let blank = type_pointer(
            &fake,
            owned,
            SendReading::for_test(InputState::Empty),
            " \u{a0}\n",
        );
        assert!(matches!(blank, Err(WakeError::EmptyMessage(_))));
        assert!(fake.calls().is_empty());
        Ok(())
    }
}
