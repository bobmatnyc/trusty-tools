//! Slack binding for the channel-agnostic session-manager proxy (TELUI-6, #2549).
//!
//! Why: the focus state machine and both proxy directions (INJECT free text →
//! session send, SUMMARIZE session activity → channel) are channel-agnostic and
//! live in [`crate::client::proxy`] so Slack reuses the SAME [`SessionProxy`] the
//! Telegram binding (`telegram::focus`) and the daemon's local HTTP proxy routes
//! use. This module is the THIN Slack binding: it maps a Slack conversation
//! (channel, or channel+thread) to the proxy's conversation key and renders the
//! proxy's structured outcomes into Slack `mrkdwn`. No focus state or routing
//! logic lives here — only presentation and the per-conversation key convention.
//! What: [`handle_focus`]/[`handle_unfocus`]/[`handle_summary`]/[`inject_reply`]
//! call the shared [`SessionProxy`] and render its outcome as a `mrkdwn` reply
//! body. [`conv`] is the Slack conversation-key convention (channel id, plus the
//! thread timestamp when the message is in a thread). [`ProxyVerb`]/[`proxy_verb`]
//! classify the three slash verbs (`/focus`, `/unfocus`, `/summary`) the adapter
//! routes to the proxy instead of projecting onto a [`crate::client::TrustyCommand`].
//! Test: `focus/tests.rs` covers the render mapping for each outcome and the
//! verb/key conventions; the state machine and daemon paths are covered by
//! `client::proxy::tests`, and the inbound-event routing by `slack::tests`.

use crate::client::{FocusOutcome, InjectOutcome, SessionProxy, SummarizeOutcome};

use super::formatter::short_id;

/// The prompt shown when a proxy action needs a focused session but none is set.
///
/// Why: `mrkdwn` uses backticks for inline code (not Telegram's `<code>`), so the
/// hint is authored in Slack's own markup rather than HTML.
/// What: a `mrkdwn` string naming the `/focus` verb the operator must run first.
/// Test: `render_inject_no_focus_hints`, `render_summary_no_focus_hints`.
const NO_FOCUS_HINT: &str = "No session is focused — use `/focus <session>` first.";

/// Safety ceiling (raw chars) for the activity summary text in a `/summary` reply.
///
/// Why: Slack's `chat.postMessage` `text` caps at 40,000 chars; a runaway
/// activity digest could exceed it and be rejected. Today's digest contract (a
/// lightweight [`crate::client::ActivityDigest`]) is always short, so this is a
/// DEFENSIVE ceiling for a path that should never be hit — but it is enforced,
/// not merely assumed (mirrors the Telegram binding's `MAX_SUMMARY_CHARS`, sized
/// for Slack's far larger budget).
/// What: the raw-text budget before the summary is embedded in the reply,
/// comfortably below Slack's 40,000-char message limit.
/// Test: `truncate_summary_leaves_short_text_untouched`,
/// `truncate_summary_caps_long_text`.
const MAX_SUMMARY_CHARS: usize = 2000;

/// Truncate `s` to at most [`MAX_SUMMARY_CHARS`] characters, marking truncation.
///
/// Why: factored out of [`render_summary`] so the truncation rule is
/// unit-testable independent of the full `mrkdwn` render.
/// What: returns `s` unchanged (borrowed, no allocation) when within budget;
/// otherwise the first [`MAX_SUMMARY_CHARS`] chars (on a char boundary) plus a
/// `" […truncated]"` marker.
/// Test: `truncate_summary_leaves_short_text_untouched`,
/// `truncate_summary_caps_long_text`.
fn truncate_summary(s: &str) -> std::borrow::Cow<'_, str> {
    if s.chars().count() <= MAX_SUMMARY_CHARS {
        return std::borrow::Cow::Borrowed(s);
    }
    let truncated: String = s.chars().take(MAX_SUMMARY_CHARS).collect();
    std::borrow::Cow::Owned(format!("{truncated} […truncated]"))
}

/// Map a Slack conversation (channel + optional thread) to the proxy's key.
///
/// Why: the proxy keys focus by an opaque channel-supplied string. Slack's
/// conversation identity is the channel id, refined to a thread when a message is
/// posted in one — so a focus set in a thread is scoped to that thread while a
/// focus in the main channel is scoped to the channel. Converting once here keeps
/// the convention out of the handlers.
/// What: returns `channel` when `thread` is `None`, else `channel:thread_ts`.
/// Test: `conv_keys_by_channel_and_thread`.
pub fn conv(channel: &str, thread: Option<&str>) -> String {
    match thread {
        Some(ts) if !ts.is_empty() => format!("{channel}:{ts}"),
        _ => channel.to_string(),
    }
}

/// The three slash verbs the Slack adapter routes to the session-manager PROXY.
///
/// Why: `/focus`, `/unfocus`, and `/summary` drive the per-conversation focus
/// state machine, NOT the daemon command surface — modelling them as a typed verb
/// keeps the adapter's dispatch a thin, exhaustive match instead of scattering
/// string compares (mirrors how the Telegram binding special-cases the same three
/// commands in `on_message`).
/// What: one variant per proxy verb; [`proxy_verb`] parses a raw Slack `command`
/// field into `Some(_)`, or `None` for any other verb (which then falls through
/// to normal `parse_slash` dispatch or free-text routing).
/// Test: `proxy_verb_classifies_the_three_verbs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyVerb {
    /// `/focus [session]` — focus a managed session for the conversation.
    Focus,
    /// `/unfocus` — clear the conversation's focus.
    Unfocus,
    /// `/summary` — digest the focused session's activity back to the channel.
    Summary,
}

/// Classify a raw Slack slash `command` as a proxy verb, if it is one.
///
/// Why: the adapter must intercept the three proxy verbs BEFORE projecting a
/// slash command onto a [`crate::client::TrustyCommand`], since they have no
/// command-surface equivalent. Centralizing the match keeps the routing thin and
/// the leading-slash / case handling identical to `commands::parse_slash`.
/// What: strips an optional leading `/`, lowercases, and matches `focus` /
/// `unfocus` / `summary`; returns `None` for anything else.
/// Test: `proxy_verb_classifies_the_three_verbs`.
pub fn proxy_verb(command: &str) -> Option<ProxyVerb> {
    match command
        .trim()
        .trim_start_matches('/')
        .to_ascii_lowercase()
        .as_str()
    {
        "focus" => Some(ProxyVerb::Focus),
        "unfocus" => Some(ProxyVerb::Unfocus),
        "summary" => Some(ProxyVerb::Summary),
        _ => None,
    }
}

/// Handle `/focus [session]` for a Slack conversation `conv`.
///
/// Why: focusing routes through the shared proxy (which validates the target and
/// captures its id/name); this binding only renders the outcome as Slack `mrkdwn`.
/// What: calls [`SessionProxy::focus`] and renders the [`FocusOutcome`].
/// Test: `handle_focus_empty_hints`, `render_focus_focused_names_session`.
pub async fn handle_focus(proxy: &SessionProxy, conv: &str, arg: &str) -> String {
    render_focus(&proxy.focus(conv, arg).await)
}

/// Handle `/unfocus` for a Slack conversation `conv`.
///
/// Why: returns the conversation to fleet-wide chat; the reply names the cleared
/// session so the state change is unambiguous.
/// What: clears the proxy focus and renders a confirmation (or a no-op notice).
/// Test: `handle_unfocus_when_none`, `slash_unfocus_reaches_proxy`.
pub fn handle_unfocus(proxy: &SessionProxy, conv: &str) -> String {
    match proxy.unfocus(conv) {
        Some(f) => format!(
            "✅ Unfocused *{}*. Plain messages now go to the coordinator.",
            f.name,
        ),
        None => "No session was focused.".to_string(),
    }
}

/// Handle `/summary` for a Slack conversation `conv` — digest the session.
///
/// Why: the SUMMARIZE proxy direction on the Slack surface — read back what the
/// focused session is doing without attaching to tmux.
/// What: calls [`SessionProxy::summarize`] and renders the [`SummarizeOutcome`].
/// Test: `render_summary_ok_shows_state_and_pending`, `slash_summary_reaches_proxy_activity`.
pub async fn handle_summary(proxy: &SessionProxy, conv: &str) -> String {
    render_summary(&proxy.summarize(conv).await)
}

/// Route a free-text message to the focused session (INJECT) and render the ack.
///
/// Why: the payoff of focus mode on Slack — a plain message becomes a send at the
/// focused session; the proxy handles resolution and dead-session auto-unfocus,
/// this binding renders the result.
/// What: calls [`SessionProxy::inject`] and renders the [`InjectOutcome`].
/// Test: `render_inject_sent_echoes_text`, `message_when_focused_reaches_proxy_send`.
pub async fn inject_reply(proxy: &SessionProxy, conv: &str, text: &str) -> String {
    render_inject(&proxy.inject(conv, text).await)
}

/// Render a [`FocusOutcome`] as a Slack `mrkdwn` reply.
///
/// Test: `render_focus_focused_names_session`, `render_focus_current_none_hints_usage`,
/// `render_focus_not_found_reports`.
fn render_focus(outcome: &FocusOutcome) -> String {
    match outcome {
        FocusOutcome::Focused(f) => format!(
            "🎯 Focused on *{}* (`{}`).\n\
             Plain messages now route to this session; `/unfocus` to stop.",
            f.name,
            short_id(&f.id),
        ),
        FocusOutcome::Current(Some(f)) => format!(
            "🎯 Focused on *{}* (`{}`).\n\
             Plain messages route here; `/unfocus` to stop.",
            f.name,
            short_id(&f.id),
        ),
        FocusOutcome::Current(None) => "Usage: `/focus <session>` — focus a managed session so \
             plain messages route straight to it."
            .to_string(),
        FocusOutcome::NotFound { target, error } => {
            format!("❌ Cannot focus `{target}`: {error}")
        }
    }
}

/// Render an [`InjectOutcome`] as a Slack `mrkdwn` reply.
///
/// Test: `render_inject_sent_echoes_text`, `render_inject_auto_unfocused_signals_gone`,
/// `render_inject_failed_keeps_focus_prose`, `render_inject_no_focus_hints`.
fn render_inject(outcome: &InjectOutcome) -> String {
    match outcome {
        InjectOutcome::Sent { target, text } => format!(
            "📨 → *{}* (`{}`)\n_{}_",
            target.name,
            short_id(&target.id),
            text,
        ),
        InjectOutcome::AutoUnfocused { target, error } => format!(
            "⚠️ Focused session *{}* is gone — focus cleared, back to the coordinator. ({})",
            target.name, error,
        ),
        InjectOutcome::Failed { target, error } => format!(
            "❌ Send to *{}* failed: {}\nStill focused; `/unfocus` to stop.",
            target.name, error,
        ),
        InjectOutcome::NoFocus => NO_FOCUS_HINT.to_string(),
    }
}

/// Render a [`SummarizeOutcome`] as a Slack `mrkdwn` reply.
///
/// Test: `render_summary_ok_shows_state_and_pending`, `render_summary_no_focus_hints`.
fn render_summary(outcome: &SummarizeOutcome) -> String {
    match outcome {
        SummarizeOutcome::Summary {
            target,
            state,
            summary,
            pending_decision,
        } => {
            let decision = pending_decision
                .as_deref()
                .map(|d| format!("\n⚠️ pending: {d}"))
                .unwrap_or_default();
            format!(
                "*📋 {}* (`{}`) [{}]\n{}{decision}",
                target.name,
                short_id(&target.id),
                state,
                truncate_summary(summary),
            )
        }
        SummarizeOutcome::AutoUnfocused { target, error } => format!(
            "⚠️ Focused session *{}* is gone — focus cleared, back to the coordinator. ({})",
            target.name, error,
        ),
        SummarizeOutcome::Failed { target, error } => {
            format!("❌ Summary of *{}* failed: {}", target.name, error)
        }
        SummarizeOutcome::NoFocus => NO_FOCUS_HINT.to_string(),
    }
}

#[cfg(test)]
mod tests;
