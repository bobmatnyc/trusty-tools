//! Telegram binding for the channel-agnostic session-manager proxy (TELUI-6).
//!
//! Why: the focus state machine and both proxy directions (INJECT free text →
//! session send, SUMMARIZE session activity → channel) are channel-agnostic and
//! live in [`crate::client::proxy`] so Slack and an MCP surface reuse them. This
//! module is the THIN Telegram binding: it maps a Telegram `chat_id` to the
//! proxy's conversation key and renders the proxy's structured outcomes into
//! Telegram HTML. No focus state or routing logic lives here — only presentation
//! and the per-chat conversation-key convention.
//! What: [`handle_focus`]/[`handle_unfocus`]/[`handle_summary`]/[`inject_reply`]
//! call the shared [`SessionProxy`] and render its outcome as an HTML reply
//! body. [`conv`] is the Telegram conversation-key convention (`chat_id` as a
//! string). The routing decision is re-exported from the proxy so `on_message`
//! uses the one shared function.
//! Test: `focus/tests.rs` covers the render mapping for each outcome; the state
//! machine and daemon paths are covered by `client::proxy::tests`.

use crate::client::{FocusOutcome, InjectOutcome, SessionProxy, SummarizeOutcome};

use super::formatter::{html_escape, short_id};

/// The prompt shown when a proxy action needs a focused session but none is set.
const NO_FOCUS_HINT: &str =
    "No session is focused — use <code>/focus &lt;session&gt;</code> first.";

/// Safety ceiling (raw, pre-HTML-escape chars) for the activity summary text
/// embedded in a `/summary` reply.
///
/// Why: [`html_escape`] can expand a string up to ~5x (`&` → `&amp;`); an
/// unbounded activity digest could push a reply over Telegram's hard 4096-char
/// message-body limit, silently dropping the message. Today's digest contract
/// (a lightweight [`crate::client::ActivityDigest`]) is always short, so this is
/// a DEFENSIVE ceiling for a path that should never be hit, not an expected
/// truncation — but it is enforced, not merely assumed.
/// What: the raw-text budget before escaping; comfortably below the 4096-char
/// limit even at worst-case 5x escape expansion plus the surrounding template.
/// Test: `truncate_summary_leaves_short_text_untouched`,
/// `truncate_summary_caps_long_text`.
const MAX_SUMMARY_CHARS: usize = 700;

/// Truncate `s` to at most [`MAX_SUMMARY_CHARS`] characters, marking truncation.
///
/// Why: factored out of [`render_summary`] so the truncation rule is
/// unit-testable independent of the full HTML render.
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

/// Map a Telegram `chat_id` to the proxy's conversation key.
///
/// Why: the proxy keys focus by an opaque channel-supplied string; Telegram's
/// per-chat conversation is identified by its `chat_id`, so the binding converts
/// once here rather than scattering `to_string()` calls through the handlers.
/// What: returns the `chat_id` rendered as a decimal string.
/// Test: covered indirectly by every handler test.
pub fn conv(chat_id: i64) -> String {
    chat_id.to_string()
}

/// Handle `/focus [session]` (and the `🎯 Focus` button) for `chat_id`.
///
/// Why: focusing routes through the shared proxy (which validates the target and
/// captures its id/name); this binding only renders the outcome for Telegram.
/// What: calls [`SessionProxy::focus`] and renders the [`FocusOutcome`] as HTML.
/// Test: `handle_focus_empty_hints`, `render_focus_*`.
pub async fn handle_focus(proxy: &SessionProxy, chat_id: i64, arg: &str) -> String {
    render_focus(&proxy.focus(&conv(chat_id), arg).await)
}

/// Handle `/unfocus` for `chat_id`.
///
/// Why: returns the conversation to fleet-wide chat; the reply names the cleared
/// session so the state change is unambiguous.
/// What: clears the proxy focus and renders a confirmation (or a no-op notice).
/// Test: `handle_unfocus_clears_and_reports`, `handle_unfocus_when_none`.
pub fn handle_unfocus(proxy: &SessionProxy, chat_id: i64) -> String {
    match proxy.unfocus(&conv(chat_id)) {
        Some(f) => format!(
            "✅ Unfocused <b>{}</b>. Plain messages now go to the coordinator.",
            html_escape(&f.name),
        ),
        None => "No session was focused.".to_string(),
    }
}

/// Handle `/summary` for `chat_id` — digest the focused session's activity.
///
/// Why: the SUMMARIZE proxy direction on the Telegram surface — read back what
/// the focused session is doing without attaching to tmux.
/// What: calls [`SessionProxy::summarize`] and renders the [`SummarizeOutcome`].
/// Test: `render_summary_*`.
pub async fn handle_summary(proxy: &SessionProxy, chat_id: i64) -> String {
    render_summary(&proxy.summarize(&conv(chat_id)).await)
}

/// Route a free-text message to the focused session (INJECT) and render the ack.
///
/// Why: the payoff of focus mode on Telegram — a plain message becomes a send at
/// the focused session; the proxy handles resolution and dead-session
/// auto-unfocus, this binding renders the result.
/// What: calls [`SessionProxy::inject`] and renders the [`InjectOutcome`].
/// Test: `render_inject_*`.
pub async fn inject_reply(proxy: &SessionProxy, chat_id: i64, text: &str) -> String {
    render_inject(&proxy.inject(&conv(chat_id), text).await)
}

/// Render a [`FocusOutcome`] as a Telegram HTML reply.
///
/// Test: `render_focus_focused`, `render_focus_current_none`, `render_focus_not_found`.
fn render_focus(outcome: &FocusOutcome) -> String {
    match outcome {
        FocusOutcome::Focused(f) => format!(
            "🎯 Focused on <b>{}</b> (<code>{}</code>).\n\
             Plain messages now route to this session; /unfocus to stop.",
            html_escape(&f.name),
            short_id(&f.id),
        ),
        FocusOutcome::Current(Some(f)) => format!(
            "🎯 Focused on <b>{}</b> (<code>{}</code>).\n\
             Plain messages route here; /unfocus to stop.",
            html_escape(&f.name),
            short_id(&f.id),
        ),
        FocusOutcome::Current(None) => "Usage: <code>/focus &lt;session&gt;</code> — focus a \
             managed session so plain messages route straight to it."
            .to_string(),
        FocusOutcome::NotFound { target, error } => format!(
            "❌ Cannot focus <code>{}</code>: {}",
            html_escape(target),
            html_escape(error),
        ),
    }
}

/// Render an [`InjectOutcome`] as a Telegram HTML reply.
///
/// Test: `render_inject_sent`, `render_inject_auto_unfocused`,
/// `render_inject_failed`, `render_inject_no_focus`.
fn render_inject(outcome: &InjectOutcome) -> String {
    match outcome {
        InjectOutcome::Sent { target, text } => format!(
            "📨 → <b>{}</b> (<code>{}</code>)\n<i>{}</i>",
            html_escape(&target.name),
            short_id(&target.id),
            html_escape(text),
        ),
        InjectOutcome::AutoUnfocused { target, error } => format!(
            "⚠️ Focused session <b>{}</b> is gone — focus cleared, back to the \
             coordinator. ({})",
            html_escape(&target.name),
            html_escape(error),
        ),
        InjectOutcome::Failed { target, error } => format!(
            "❌ Send to <b>{}</b> failed: {}\nStill focused; /unfocus to stop.",
            html_escape(&target.name),
            html_escape(error),
        ),
        InjectOutcome::NoFocus => NO_FOCUS_HINT.to_string(),
    }
}

/// Render a [`SummarizeOutcome`] as a Telegram HTML reply.
///
/// Test: `render_summary_ok`, `render_summary_no_focus`.
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
                .map(|d| format!("\n⚠️ pending: {}", html_escape(d)))
                .unwrap_or_default();
            format!(
                "<b>📋 {}</b> (<code>{}</code>) [{}]\n{}{decision}",
                html_escape(&target.name),
                short_id(&target.id),
                html_escape(state),
                html_escape(&truncate_summary(summary)),
            )
        }
        SummarizeOutcome::AutoUnfocused { target, error } => format!(
            "⚠️ Focused session <b>{}</b> is gone — focus cleared, back to the \
             coordinator. ({})",
            html_escape(&target.name),
            html_escape(error),
        ),
        SummarizeOutcome::Failed { target, error } => format!(
            "❌ Summary of <b>{}</b> failed: {}",
            html_escape(&target.name),
            html_escape(error),
        ),
        SummarizeOutcome::NoFocus => NO_FOCUS_HINT.to_string(),
    }
}

#[cfg(test)]
mod tests;
