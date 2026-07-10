//! Channel-agnostic session-manager PROXY layer (TELUI-6, #1440).
//!
//! Why: this is layer 2 of the three-layer control model — the session-manager
//! acting as a PROXY between an external channel (Telegram, Slack, MCP) and ONE
//! managed session. Layer 1 is direct tmux attach; layer 3 (the cross-scope
//! project manager, #2109) is out of scope here. The proxy is deliberately
//! SINGLE-SESSION-focused: a conversation "focuses" one session and then talks
//! to it, in two directions — INJECT (free text routed to the session's
//! `managed-send`) and SUMMARIZE (the session's recent activity digested back to
//! the channel). Putting the focus state machine and both directions HERE, in the
//! shared chat-core over the one [`CommandExecutor`], means Slack and an MCP
//! surface bind to the same API instead of re-implementing the state machine —
//! each channel only supplies its conversation key and renders the structured
//! outcomes in its own markup.
//! What: [`SessionProxy`] owns the per-conversation focus map (keyed by an opaque
//! channel-supplied conversation string — Telegram passes its `chat_id`, Slack a
//! channel id, MCP a client/session id) and exposes [`SessionProxy::focus`],
//! [`SessionProxy::unfocus`], [`SessionProxy::current_focus`],
//! [`SessionProxy::inject`], and [`SessionProxy::summarize`], each returning a
//! structured, channel-agnostic outcome the binding renders. [`route_free_text`]
//! is the pure inject-vs-coordinator routing decision. Focus state is in-memory
//! and VOLATILE across a daemon restart (the channels persist no per-conversation
//! state); a restart simply drops every conversation back to fleet-wide chat.
//! Test: `proxy/tests.rs` covers the pure routing, the store round-trip, and the
//! inject/summarize/dead-session paths against an in-process test daemon.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::executor::CommandExecutor;
use super::{CommandResult, TrustyCommand};

/// A resolved focused session: its canonical id plus a display name.
///
/// Why: the proxy keeps the resolved id (so later inject/summarize skip
/// re-resolution and are unambiguous) AND the friendly name (so a channel can
/// name the session in replies without another daemon round-trip).
/// What: the canonical managed-session `id` and its human-readable `name`, both
/// captured at focus time from the authoritative `managed-get` record.
/// Test: `set_get_clear_round_trip` in `proxy/tests.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FocusTarget {
    /// Canonical managed-session id (a UUID).
    pub id: String,
    /// Friendly session name shown in replies.
    pub name: String,
}

/// The routing decision for one free-text (non-command) message.
///
/// Why: modelling the two outcomes keeps each channel's handler branch trivial
/// and the decision itself pure and unit-testable, shared across channels.
/// What: `Inject` routes the text to the focused session's `managed-send`;
/// `Coordinator` routes it to the action-capable coordinator (the default).
/// Test: the `route_free_text_*` tests in `proxy/tests.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreeTextRoute {
    /// Route the message to the focused session (proxy INJECT direction).
    Inject,
    /// Route the message to the action-capable coordinator.
    Coordinator,
}

/// Outcome of a [`SessionProxy::focus`] request, for the channel to render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FocusOutcome {
    /// The session was validated and is now focused for the conversation.
    Focused(FocusTarget),
    /// No target was supplied — reports the current focus (or `None`).
    Current(Option<FocusTarget>),
    /// The target could not be resolved; focus is unchanged.
    NotFound {
        /// The unresolved target the caller asked to focus.
        target: String,
        /// The daemon/resolver error explaining why.
        error: String,
    },
}

/// Outcome of a [`SessionProxy::inject`] (free text → session send).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InjectOutcome {
    /// The text was sent to the focused session.
    Sent {
        /// The focused session it was sent to.
        target: FocusTarget,
        /// The text that was injected.
        text: String,
    },
    /// The focused session no longer exists — focus was auto-cleared.
    AutoUnfocused {
        /// The session that was focused (now cleared).
        target: FocusTarget,
        /// The "not found" error that triggered the auto-unfocus.
        error: String,
    },
    /// A transient failure — focus is preserved so a blip loses no context.
    Failed {
        /// The still-focused session.
        target: FocusTarget,
        /// The transport/daemon error.
        error: String,
    },
    /// No session was focused for the conversation.
    NoFocus,
}

/// Outcome of a [`SessionProxy::summarize`] (session activity → channel).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SummarizeOutcome {
    /// A digest of the focused session's recent activity.
    Summary {
        /// The focused session summarized.
        target: FocusTarget,
        /// The session's current lifecycle state.
        state: String,
        /// The activity summary text.
        summary: String,
        /// Any decision the session is blocked on.
        pending_decision: Option<String>,
    },
    /// The focused session no longer exists — focus was auto-cleared.
    AutoUnfocused {
        /// The session that was focused (now cleared).
        target: FocusTarget,
        /// The "not found" error that triggered the auto-unfocus.
        error: String,
    },
    /// A transient failure — focus is preserved.
    Failed {
        /// The still-focused session.
        target: FocusTarget,
        /// The transport/daemon error.
        error: String,
    },
    /// No session was focused for the conversation.
    NoFocus,
}

/// Decide where a free-text message routes, given whether a session is focused.
///
/// Why: the single seam between "channel received a non-command message" and
/// "which subsystem handles it"; pure (no store, no daemon) so every branch is
/// testable and identical across channels. It runs only after a channel has
/// already failed to parse the text as a known command, so a leading `/` here is
/// an UNKNOWN command — never hijacked into the focused session (that would send
/// a mistyped command as a prompt); such lines fall through to the coordinator.
/// What: a `/`-prefixed line always routes to [`FreeTextRoute::Coordinator`];
/// otherwise a focused conversation routes to [`FreeTextRoute::Inject`] and an
/// unfocused one to [`FreeTextRoute::Coordinator`].
/// Test: `route_free_text_focused_injects`, `route_free_text_unfocused_coordinates`,
/// `route_free_text_slash_never_injects`.
pub fn route_free_text(text: &str, has_focus: bool) -> FreeTextRoute {
    if text.trim_start().starts_with('/') {
        return FreeTextRoute::Coordinator;
    }
    if has_focus {
        FreeTextRoute::Inject
    } else {
        FreeTextRoute::Coordinator
    }
}

/// Channel-agnostic session-manager proxy: per-conversation focus + inject +
/// summarize over the shared [`CommandExecutor`].
///
/// Why: one implementation of layer 2 that every channel binds to; the focus
/// state machine and the two proxy directions live here, not in each adapter.
/// What: holds an `Arc<CommandExecutor>` (the shared daemon seam) and a
/// `Mutex`-guarded map of conversation key → [`FocusTarget`]. Construct one per
/// channel and share it (`Arc`) across that channel's handler tasks.
/// Test: `proxy/tests.rs`.
pub struct SessionProxy {
    executor: Arc<CommandExecutor>,
    focus: Mutex<HashMap<String, FocusTarget>>,
}

impl SessionProxy {
    /// Build a proxy over the shared executor.
    ///
    /// Why: each channel already holds an `Arc<CommandExecutor>`; the proxy
    /// borrows the same seam so there is one daemon transport per channel.
    /// What: wraps the executor and an empty focus map.
    /// Test: used by every `proxy/tests.rs` test.
    pub fn new(executor: Arc<CommandExecutor>) -> Self {
        Self {
            executor,
            focus: Mutex::new(HashMap::new()),
        }
    }

    /// Read the focused session for `conv`, if any.
    ///
    /// Why: a channel needs to know whether a conversation is focused (to route
    /// free text) and to name the focused session in replies.
    /// What: returns a clone of the [`FocusTarget`] under `conv`, or `None`.
    /// Test: `set_get_clear_round_trip`.
    pub fn current_focus(&self, conv: &str) -> Option<FocusTarget> {
        self.lock().get(conv).cloned()
    }

    /// Clear the focus for `conv`, returning the session that was focused.
    ///
    /// Why: the "back to fleet chat" action (`/unfocus`) and the dead-session
    /// auto-unfocus both remove the entry and report which session was cleared.
    /// What: removes and returns the [`FocusTarget`] under `conv`, or `None`.
    /// Test: `set_get_clear_round_trip`, `inject_auto_unfocuses_dead_session`.
    pub fn unfocus(&self, conv: &str) -> Option<FocusTarget> {
        self.lock().remove(conv)
    }

    /// Focus a session for `conv`, validating it exists first.
    ///
    /// Why: focusing must VALIDATE the target — focusing a typo would silently
    /// swallow every later inject into a non-existent session. Reusing the shared
    /// `managed-get` both validates and yields the canonical id + name in one
    /// call, so no channel re-implements resolution.
    /// What: with an empty `target`, returns the current focus
    /// ([`FocusOutcome::Current`]) without touching the daemon. Otherwise runs
    /// [`TrustyCommand::ManagedGet`]; on success it records the resolved id/name
    /// and returns [`FocusOutcome::Focused`]; on failure it returns
    /// [`FocusOutcome::NotFound`] and leaves any existing focus untouched.
    /// Test: `focus_empty_reports_current`, `focus_unknown_is_not_found`.
    pub async fn focus(&self, conv: &str, target: &str) -> FocusOutcome {
        let target = target.trim();
        if target.is_empty() {
            return FocusOutcome::Current(self.current_focus(conv));
        }
        match self
            .executor
            .execute(TrustyCommand::ManagedGet {
                target: target.to_string(),
            })
            .await
        {
            CommandResult::ManagedSession(view) => {
                let ft = FocusTarget {
                    id: view.id,
                    name: view.name,
                };
                self.lock().insert(conv.to_string(), ft.clone());
                FocusOutcome::Focused(ft)
            }
            CommandResult::Error(error) => FocusOutcome::NotFound {
                target: target.to_string(),
                error,
            },
            _ => FocusOutcome::NotFound {
                target: target.to_string(),
                error: "unexpected daemon response".to_string(),
            },
        }
    }

    /// INJECT: route free text to the focused session's `managed-send`.
    ///
    /// Why: the payoff of focus mode — a plain message becomes a `send` at the
    /// focused session with no per-turn id ceremony. A vanished session (the
    /// resolver reports "not found") auto-unfocuses so the conversation drops back
    /// to fleet chat instead of failing forever; a transient error keeps the focus
    /// so a blip loses no context.
    /// What: with no focus returns [`InjectOutcome::NoFocus`]; otherwise runs
    /// [`TrustyCommand::ManagedSend`] at the focused id and maps the result to
    /// `Sent` / `AutoUnfocused` (missing session, focus cleared) / `Failed`.
    /// Test: `inject_no_focus`, `inject_auto_unfocuses_dead_session`,
    /// `inject_transient_error_keeps_focus`.
    pub async fn inject(&self, conv: &str, text: &str) -> InjectOutcome {
        let Some(focus) = self.current_focus(conv) else {
            return InjectOutcome::NoFocus;
        };
        let result = self
            .executor
            .execute(TrustyCommand::ManagedSend {
                target: focus.id.clone(),
                text: text.to_string(),
            })
            .await;
        match result {
            CommandResult::ManagedSent { .. } => InjectOutcome::Sent {
                target: focus,
                text: text.to_string(),
            },
            CommandResult::Error(error) if is_missing_session(&error) => {
                self.unfocus(conv);
                InjectOutcome::AutoUnfocused {
                    target: focus,
                    error,
                }
            }
            CommandResult::Error(error) => InjectOutcome::Failed {
                target: focus,
                error,
            },
            _ => InjectOutcome::Failed {
                target: focus,
                error: "unexpected daemon response".to_string(),
            },
        }
    }

    /// SUMMARIZE: digest the focused session's recent activity back to the channel.
    ///
    /// Why: the second proxy direction — the operator asks "what is my session
    /// doing?" and the proxy relays a digest without them attaching to tmux. This
    /// is the minimal digest built on the existing activity surface; a richer
    /// LLM-summarized digest is follow-up (see the PR "Layering" notes).
    /// What: with no focus returns [`SummarizeOutcome::NoFocus`]; otherwise runs
    /// [`TrustyCommand::ManagedActivity`] and maps the result to `Summary` /
    /// `AutoUnfocused` (missing session, focus cleared) / `Failed`.
    /// Test: `summarize_no_focus`, `summarize_auto_unfocuses_dead_session`.
    pub async fn summarize(&self, conv: &str) -> SummarizeOutcome {
        let Some(focus) = self.current_focus(conv) else {
            return SummarizeOutcome::NoFocus;
        };
        let result = self
            .executor
            .execute(TrustyCommand::ManagedActivity {
                target: focus.id.clone(),
            })
            .await;
        match result {
            CommandResult::ManagedActivity {
                state,
                summary,
                pending_decision,
                ..
            } => SummarizeOutcome::Summary {
                target: focus,
                state,
                summary,
                pending_decision,
            },
            CommandResult::Error(error) if is_missing_session(&error) => {
                self.unfocus(conv);
                SummarizeOutcome::AutoUnfocused {
                    target: focus,
                    error,
                }
            }
            CommandResult::Error(error) => SummarizeOutcome::Failed {
                target: focus,
                error,
            },
            _ => SummarizeOutcome::Failed {
                target: focus,
                error: "unexpected daemon response".to_string(),
            },
        }
    }

    /// Lock the focus map, panicking only on a poisoned mutex (a programmer error
    /// — another thread panicked holding the lock).
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, FocusTarget>> {
        self.focus.lock().expect("focus map mutex poisoned")
    }
}

/// Whether a `managed-*` error means the focused session no longer exists.
///
/// Why: the auto-unfocus path must fire ONLY when the session is genuinely gone,
/// not on a transient transport error — unfocusing on a network blip would throw
/// away the operator's context. The resolver reports a missing target as
/// "managed session <target> not found", so that substring is the signal.
/// What: returns true when `msg` contains "not found".
/// Test: `is_missing_session_detects_not_found`.
fn is_missing_session(msg: &str) -> bool {
    msg.contains("not found")
}

#[cfg(test)]
mod tests;
