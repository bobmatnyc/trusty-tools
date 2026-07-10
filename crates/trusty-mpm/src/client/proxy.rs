//! Channel-agnostic session-manager PROXY layer (TELUI-6, #1440).
//!
//! Why: this is layer 2 of the three-layer control model — the session-manager
//! acting as a PROXY between an external channel (Telegram, Slack, a local HTTP
//! caller) and ONE managed session. Layer 1 is direct tmux attach; layer 3 (the
//! cross-scope project manager, #2109) is out of scope here. The proxy is
//! deliberately SINGLE-SESSION-focused: a conversation "focuses" one session and
//! then talks to it, in two directions — INJECT (free text routed to the
//! session's `managed-send`) and SUMMARIZE (the session's recent activity
//! digested back to the channel). Putting the focus state machine and both
//! directions HERE, behind a [`ManagedBackend`] abstraction, means every surface
//! — Telegram, Slack, and the daemon's OWN local HTTP proxy routes
//! (`daemon::managed_routes::proxy`) — share the identical [`SessionProxy`]
//! state machine and differ only in HOW they reach a managed session: over HTTP
//! via [`super::executor::CommandExecutor`] (external channels) or in-process via
//! the daemon's `SessionManager` (the local proxy routes). A caller can therefore
//! exercise this entire state machine with `curl` against the daemon before ever
//! wiring up a Telegram bot token.
//! What: [`SessionProxy`] owns the per-conversation focus map (keyed by an opaque
//! channel-supplied conversation string — Telegram passes its `chat_id`, Slack a
//! channel id, the local HTTP surface a caller-supplied `conversation_key`) and
//! exposes [`SessionProxy::focus`], [`SessionProxy::unfocus`],
//! [`SessionProxy::current_focus`], [`SessionProxy::inject`], and
//! [`SessionProxy::summarize`], each returning a structured, channel-agnostic
//! outcome the binding renders. [`route_free_text`] is the pure inject-vs-
//! coordinator routing decision. Focus state is in-memory and VOLATILE across a
//! daemon restart (no channel persists per-conversation state); a restart simply
//! drops every conversation back to fleet-wide chat.
//! Test: `proxy/tests.rs` covers the pure routing, the store round-trip, and the
//! inject/summarize/dead-session paths against an in-process test daemon (via the
//! `CommandExecutor` backend); `daemon::managed_routes::proxy` and
//! `tests/proxy_routes.rs` cover the SAME state machine reached through the
//! direct in-process backend the daemon's local HTTP surface uses.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;

use super::executor::CommandExecutor;
use super::{CommandResult, TrustyCommand};

/// A resolved focused session: its canonical id plus a display name.
///
/// Why: the proxy keeps the resolved id (so later inject/summarize skip
/// re-resolution and are unambiguous) AND the friendly name (so a channel can
/// name the session in replies without another daemon round-trip).
/// What: the canonical managed-session `id` and its human-readable `name`, both
/// captured at focus time from the authoritative resolution the backend performs.
/// Test: `set_get_clear_round_trip` in `proxy/tests.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FocusTarget {
    /// Canonical managed-session id (a UUID string).
    pub id: String,
    /// Friendly session name shown in replies.
    pub name: String,
}

/// A lightweight digest of a managed session's current activity.
///
/// Why: [`SessionProxy::summarize`] needs just enough to answer "what is my
/// focused session doing?" without forcing every [`ManagedBackend`] to speak the
/// full, heavier `ManagedActivity` wire shape (token counts, cache-hit flags,
/// etc. — those stay on the richer `GET .../activity` endpoint UIs can call
/// directly when they want the full picture).
/// What: the session's lifecycle state, a short human-readable summary, and any
/// pending decision it is blocked on.
/// Test: exercised via each `ManagedBackend` impl's tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityDigest {
    /// Lifecycle state (e.g. `"active"`, `"stopped"`).
    pub state: String,
    /// Short human-readable summary of what the session is doing.
    pub summary: String,
    /// A pending decision question, if the session is blocked on one.
    pub pending_decision: Option<String>,
}

/// Backend abstraction the proxy resolves/sends/digests through.
///
/// Why: this is the seam that lets [`SessionProxy`] be the SAME struct and the
/// SAME state machine for every surface, while each surface reaches a managed
/// session differently — an external channel (Telegram, Slack) goes over HTTP
/// via [`CommandExecutor`]; the daemon's own local proxy routes go in-process
/// via `SessionManager` directly (see `daemon::managed_routes::proxy`), with no
/// network hop. Neither implementation duplicates the focus/inject/summarize
/// decision logic — only the three primitive operations below differ.
/// What: `resolve` maps a fuzzy target (id, friendly name, or prefix) to its
/// canonical `(id, name)`; `send` injects text into a resolved session; `activity`
/// fetches a lightweight digest. All three return `Err(String)` on failure — a
/// message a resolver-style "not found" error must phrase as containing the
/// literal substring `"not found"` so [`is_missing_session`] can distinguish a
/// vanished session from a transient transport failure (see that function's doc).
/// Test: `ManagedBackend for CommandExecutor` is exercised by `proxy/tests.rs`;
/// the daemon's direct backend by `daemon::managed_routes::proxy::tests`.
#[async_trait]
pub trait ManagedBackend: Send + Sync {
    /// Resolve a fuzzy target to its canonical `(id, name)`.
    async fn resolve(&self, target: &str) -> Result<(String, String), String>;
    /// Inject `text` into the session identified by canonical `id`.
    async fn send(&self, id: &str, text: &str) -> Result<(), String>;
    /// Fetch a lightweight activity digest for the session identified by `id`.
    async fn activity(&self, id: &str) -> Result<ActivityDigest, String>;
}

/// [`ManagedBackend`] over the shared HTTP [`CommandExecutor`] — the backend
/// every EXTERNAL channel (Telegram, Slack) uses.
///
/// Why: external channels are separate OS processes from the daemon; they can
/// only reach a managed session over the daemon's existing HTTP API, which is
/// exactly what [`CommandExecutor`] already does via [`TrustyCommand::ManagedGet`],
/// [`TrustyCommand::ManagedSend`], and [`TrustyCommand::ManagedActivity`].
/// What: maps each [`ManagedBackend`] method onto the corresponding
/// [`TrustyCommand`] and unwraps the resulting [`CommandResult`].
/// Test: `proxy/tests.rs` (via `SessionProxy` built over this backend against an
/// in-process test daemon).
#[async_trait]
impl ManagedBackend for CommandExecutor {
    async fn resolve(&self, target: &str) -> Result<(String, String), String> {
        match self
            .execute(TrustyCommand::ManagedGet {
                target: target.to_string(),
            })
            .await
        {
            CommandResult::ManagedSession(view) => Ok((view.id, view.name)),
            CommandResult::Error(e) => Err(e),
            _ => Err("unexpected daemon response".to_string()),
        }
    }

    async fn send(&self, id: &str, text: &str) -> Result<(), String> {
        match self
            .execute(TrustyCommand::ManagedSend {
                target: id.to_string(),
                text: text.to_string(),
            })
            .await
        {
            CommandResult::ManagedSent { .. } => Ok(()),
            CommandResult::Error(e) => Err(e),
            _ => Err("unexpected daemon response".to_string()),
        }
    }

    async fn activity(&self, id: &str) -> Result<ActivityDigest, String> {
        match self
            .execute(TrustyCommand::ManagedActivity {
                target: id.to_string(),
            })
            .await
        {
            CommandResult::ManagedActivity {
                state,
                summary,
                pending_decision,
                ..
            } => Ok(ActivityDigest {
                state,
                summary,
                pending_decision,
            }),
            CommandResult::Error(e) => Err(e),
            _ => Err("unexpected daemon response".to_string()),
        }
    }
}

/// The routing decision for one free-text (non-command) message.
///
/// Why: the single seam between "channel received a non-command message" and
/// "which subsystem handles it"; modelling the two outcomes keeps each channel's
/// handler branch trivial and the decision itself pure and unit-testable, shared
/// across channels.
/// What: `Inject` routes the text to the focused session (proxy INJECT
/// direction); `Coordinator` routes it to the action-capable coordinator (the
/// default when nothing is focused).
/// Test: the `route_free_text_*` tests in `proxy/tests.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreeTextRoute {
    /// Route the message to the focused session (proxy INJECT direction).
    Inject,
    /// Route the message to the action-capable coordinator.
    Coordinator,
}

/// Outcome of a [`SessionProxy::focus`] request, for the channel to render.
///
/// Why: focusing is a validating operation with three genuinely different
/// results a channel must render differently — successfully focused, a
/// read-only query of the current focus (empty target), or a target that could
/// not be resolved. Modelling them as one enum keeps every binding's render
/// function exhaustive instead of guessing from a boolean + optional error.
/// What: `Focused` on a successful resolve-and-set; `Current` when the caller
/// passed an empty target (a read-only "what's focused right now?" query, never
/// touching the backend); `NotFound` when the backend could not resolve the
/// target — focus is left unchanged in this case.
/// Test: `focus_empty_reports_current`, `focus_unknown_is_not_found` in
/// `proxy/tests.rs`; the render mapping in `telegram::focus::tests`.
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
        /// The backend error explaining why.
        error: String,
    },
}

/// Outcome of a [`SessionProxy::inject`] (free text → session send).
///
/// Why: an inject can land in one of four genuinely different states a channel
/// must handle distinctly — sent, the session having vanished (requiring an
/// auto-unfocus so the conversation does not fail forever), a transient failure
/// (which must NOT clear focus, or a network blip would discard the operator's
/// context), or no focus at all (letting the caller fall back to its own
/// coordinator instead of receiving an HTTP error). Modelling all four as one
/// enum keeps that branching explicit and exhaustive at every binding.
/// What: `Sent` on success; `AutoUnfocused` when the backend's resolve/send
/// failed with a "not found"-shaped error (see [`is_missing_session`]) — focus
/// was already cleared by the time this variant is returned; `Failed` on any
/// other backend error — focus is left untouched; `NoFocus` when the
/// conversation had nothing focused.
/// Test: `inject_no_focus`, `inject_auto_unfocuses_dead_session`,
/// `inject_transient_error_keeps_focus` in `proxy/tests.rs`.
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
///
/// Why: mirrors [`InjectOutcome`]'s four-way split for exactly the same reason —
/// a summarize call can succeed, discover the session is gone (auto-unfocus),
/// hit a transient failure (focus preserved), or have nothing focused to
/// summarize (letting the caller fall back gracefully).
/// What: `Summary` carries the [`ActivityDigest`] fields inline; `AutoUnfocused`
/// and `Failed` mirror [`InjectOutcome`]'s variants of the same name; `NoFocus`
/// when nothing is focused.
/// Test: `summarize_no_focus`, `summarize_auto_unfocuses_dead_session` in
/// `proxy/tests.rs`.
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
/// summarize over a pluggable [`ManagedBackend`].
///
/// Why: one implementation of layer 2 that every surface binds to — Telegram,
/// Slack, and the daemon's own local HTTP proxy routes all construct this SAME
/// struct, differing only in which [`ManagedBackend`] they pass in. The focus
/// state machine (validate-then-set, auto-unfocus-on-missing,
/// preserve-on-transient-error) lives here exactly once.
/// What: holds an `Arc<dyn ManagedBackend>` (the shared reach-a-session seam) and
/// a `Mutex`-guarded map of conversation key → [`FocusTarget`]. [`Self::new`]
/// gives the proxy its own fresh, unshared store (what a long-lived channel
/// process wants — Telegram/Slack construct exactly one proxy for the process
/// lifetime); [`Self::with_focus_store`] lets a caller supply an EXTERNAL shared
/// store instead, which the daemon's local proxy routes use so a fresh
/// [`SessionProxy`] can be constructed per HTTP request while the focus map
/// itself persists across requests (owned by `DaemonState`).
/// Test: `proxy/tests.rs`.
pub struct SessionProxy {
    backend: Arc<dyn ManagedBackend>,
    focus: Arc<std::sync::Mutex<HashMap<String, FocusTarget>>>,
}

impl SessionProxy {
    /// Build a proxy with its OWN fresh, unshared focus store.
    ///
    /// Why: a long-lived channel process (Telegram, Slack) constructs exactly one
    /// proxy for its whole lifetime, so an internally-owned store is all it needs.
    /// What: wraps `backend` with a new empty focus map.
    /// Test: used by every `proxy/tests.rs` test that does not need a shared store.
    pub fn new(backend: Arc<dyn ManagedBackend>) -> Self {
        Self {
            backend,
            focus: Arc::new(std::sync::Mutex::new(HashMap::new())),
        }
    }

    /// Build a proxy over an EXTERNAL, shared focus store.
    ///
    /// Why: the daemon's local HTTP proxy routes construct a fresh
    /// [`SessionProxy`] per request (see `daemon::managed_routes::proxy`) but the
    /// focus state must persist ACROSS requests — so the store itself is owned by
    /// `DaemonState` and shared in here via `Arc::clone`.
    /// What: wraps `backend` with the caller-supplied shared store.
    /// Test: `daemon::managed_routes::proxy::tests` (in-crate) and
    /// `tests/proxy_routes.rs` (HTTP-level) exercise this constructor.
    pub fn with_focus_store(
        backend: Arc<dyn ManagedBackend>,
        focus: Arc<std::sync::Mutex<HashMap<String, FocusTarget>>>,
    ) -> Self {
        Self { backend, focus }
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
    /// swallow every later inject into a non-existent session. Delegating to the
    /// backend's `resolve` both validates and yields the canonical id + name in
    /// one call, so no surface re-implements resolution.
    /// What: with an empty `target`, returns the current focus
    /// ([`FocusOutcome::Current`]) without touching the backend. Otherwise calls
    /// [`ManagedBackend::resolve`]; on success it records the resolved id/name and
    /// returns [`FocusOutcome::Focused`]; on failure it returns
    /// [`FocusOutcome::NotFound`] and leaves any existing focus untouched.
    /// Test: `focus_empty_reports_current`, `focus_unknown_is_not_found`.
    pub async fn focus(&self, conv: &str, target: &str) -> FocusOutcome {
        let target = target.trim();
        if target.is_empty() {
            return FocusOutcome::Current(self.current_focus(conv));
        }
        match self.backend.resolve(target).await {
            Ok((id, name)) => {
                let ft = FocusTarget { id, name };
                self.lock().insert(conv.to_string(), ft.clone());
                FocusOutcome::Focused(ft)
            }
            Err(error) => FocusOutcome::NotFound {
                target: target.to_string(),
                error,
            },
        }
    }

    /// INJECT: route free text to the focused session's `managed-send`.
    ///
    /// Why: the payoff of focus mode — a plain message becomes a `send` at the
    /// focused session with no per-turn id ceremony. A vanished session (the
    /// backend reports a "not found"-shaped error) auto-unfocuses so the
    /// conversation drops back to fleet chat instead of failing forever; a
    /// transient error keeps the focus so a blip loses no context.
    /// What: with no focus returns [`InjectOutcome::NoFocus`]; otherwise calls
    /// [`ManagedBackend::send`] at the focused id and maps the result to `Sent` /
    /// `AutoUnfocused` (missing session, focus cleared) / `Failed`.
    /// Test: `inject_no_focus`, `inject_auto_unfocuses_dead_session`,
    /// `inject_transient_error_keeps_focus`.
    pub async fn inject(&self, conv: &str, text: &str) -> InjectOutcome {
        let Some(focus) = self.current_focus(conv) else {
            return InjectOutcome::NoFocus;
        };
        match self.backend.send(&focus.id, text).await {
            Ok(()) => InjectOutcome::Sent {
                target: focus,
                text: text.to_string(),
            },
            Err(error) if is_missing_session(&error) => {
                self.unfocus(conv);
                InjectOutcome::AutoUnfocused {
                    target: focus,
                    error,
                }
            }
            Err(error) => InjectOutcome::Failed {
                target: focus,
                error,
            },
        }
    }

    /// SUMMARIZE: digest the focused session's recent activity back to the channel.
    ///
    /// Why: the second proxy direction — the operator asks "what is my session
    /// doing?" and the proxy relays a digest without them attaching to tmux.
    /// What: with no focus returns [`SummarizeOutcome::NoFocus`]; otherwise calls
    /// [`ManagedBackend::activity`] and maps the result to `Summary` /
    /// `AutoUnfocused` (missing session, focus cleared) / `Failed`.
    /// Test: `summarize_no_focus`, `summarize_auto_unfocuses_dead_session`.
    pub async fn summarize(&self, conv: &str) -> SummarizeOutcome {
        let Some(focus) = self.current_focus(conv) else {
            return SummarizeOutcome::NoFocus;
        };
        match self.backend.activity(&focus.id).await {
            Ok(digest) => SummarizeOutcome::Summary {
                target: focus,
                state: digest.state,
                summary: digest.summary,
                pending_decision: digest.pending_decision,
            },
            Err(error) if is_missing_session(&error) => {
                self.unfocus(conv);
                SummarizeOutcome::AutoUnfocused {
                    target: focus,
                    error,
                }
            }
            Err(error) => SummarizeOutcome::Failed {
                target: focus,
                error,
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
/// away the operator's context. Every [`ManagedBackend`] impl is contractually
/// required to phrase a resolution failure with the substring `"not found"` (the
/// [`CommandExecutor`] impl inherits this wording verbatim from the executor's
/// managed-session resolver's `"managed session {target} not found"`; the
/// daemon's direct backend mirrors it exactly). This is admittedly substring
/// matching rather than a typed signal —
/// `is_missing_session_matches_live_resolver_not_found_format` (in
/// `proxy/tests.rs`) pins the coupling to the ACTUAL resolver wording so a
/// message-text drift fails a test instead of silently disabling the
/// auto-unfocus safety net.
/// What: returns true when `msg` contains "not found".
/// Test: `is_missing_session_detects_not_found`,
/// `is_missing_session_matches_live_resolver_not_found_format`.
fn is_missing_session(msg: &str) -> bool {
    msg.contains("not found")
}

#[cfg(test)]
mod tests;
