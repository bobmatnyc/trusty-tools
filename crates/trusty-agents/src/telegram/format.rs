//! Message formatting + chunking for the Telegram gateway.
//!
//! Why: ctrl replies are Markdown-ish and may exceed Telegram's 4096-char cap.
//! These helpers convert to a safe HTML subset and split long replies on
//! newline boundaries so we never cut mid-tag or mid-UTF-8 sequence.
//! What: `send_long_html` (the only `async` entry point), plus the pure
//! `split_message`, `markdown_to_html_safe`, and their conversion helpers.
//! Test: `telegram::tests` exercises every pure function here directly.

use teloxide::prelude::*;
use teloxide::types::{ChatId, MessageId, ParseMode, ReplyParameters};
use tracing::warn;

use super::MAX_TELEGRAM_MESSAGE;

/// Send a (possibly long) HTML-formatted reply, splitting on the 4096-char
/// boundary at newlines where possible.
///
/// Why: Telegram rejects messages > 4096 chars with `MESSAGE_TOO_LONG`. We
/// split on newlines so we don't cut mid-tag (which would corrupt HTML
/// rendering). Reply parameters are attached only to the last chunk so the
/// thread is anchored to the user's message without spamming reply arrows on
/// every chunk.
/// What: Iterates `split_message` chunks, falling back to plain text if the
/// HTML send is rejected.
/// Test: Side-effect-only (network); the chunking is covered by `split_message`
/// tests.
pub(super) async fn send_long_html(bot: &Bot, chat_id: ChatId, user_msg_id: MessageId, text: &str) {
    let chunks = split_message(text, MAX_TELEGRAM_MESSAGE);
    let total = chunks.len();
    for (i, chunk) in chunks.iter().enumerate() {
        let is_last = i == total - 1;
        let mut req = bot.send_message(chat_id, chunk).parse_mode(ParseMode::Html);
        if is_last {
            req = req.reply_parameters(ReplyParameters::new(user_msg_id));
        }
        if let Err(e) = req.await {
            // Fallback: if HTML parsing fails for any reason (e.g. unbalanced
            // tags from naive markdown conversion), retry as plain text so
            // the user still gets the content.
            warn!(chat_id = %chat_id.0, error = %e, "HTML send failed; retrying as plain text");
            let plain = strip_html_tags(chunk);
            let mut retry = bot.send_message(chat_id, plain);
            if is_last {
                retry = retry.reply_parameters(ReplyParameters::new(user_msg_id));
            }
            if let Err(e2) = retry.await {
                warn!(chat_id = %chat_id.0, error = %e2, "plain-text fallback also failed");
            }
        }
    }
}

/// Split `text` into chunks of at most `max_len` chars, preferring to break
/// on newlines.
///
/// Why: Telegram's hard cap is 4096 chars. Hard-splitting mid-line yields
/// ugly output and can break HTML tags. We prefer the rightmost newline in
/// the first `max_len` chars, falling back to a hard split when there is no
/// newline (e.g. a 5000-char single line).
/// What: Returns a `Vec<String>` whose concatenation equals `text`.
/// Test: `split_message_short`, `split_message_newline_boundary`,
/// `split_message_hard_split` in `telegram::tests`.
pub(super) fn split_message(text: &str, max_len: usize) -> Vec<String> {
    if text.len() <= max_len {
        return vec![text.to_owned()];
    }
    let mut chunks = Vec::new();
    let mut remaining = text;
    while remaining.len() > max_len {
        // Find the last char boundary at-or-before max_len so we never slice
        // mid-UTF-8 sequence.
        let mut boundary = max_len;
        while boundary > 0 && !remaining.is_char_boundary(boundary) {
            boundary -= 1;
        }
        if boundary == 0 {
            // Pathological: a single char wider than max_len. Push the whole
            // remaining and bail to avoid an infinite loop.
            chunks.push(remaining.to_owned());
            return chunks;
        }
        // Prefer to break on the rightmost newline within [0, boundary).
        let split_at = match remaining[..boundary].rfind('\n') {
            Some(pos) => pos + 1,
            None => boundary,
        };
        chunks.push(remaining[..split_at].to_owned());
        remaining = &remaining[split_at..];
    }
    if !remaining.is_empty() {
        chunks.push(remaining.to_owned());
    }
    chunks
}

/// Convert ctrl's Markdown-ish output into Telegram-safe HTML.
///
/// Why: ctrl replies are Markdown (with code fences, inline code, bold). The
/// safest rendering on Telegram is HTML — but we have to escape user-supplied
/// content first, then re-introduce a small whitelist of formatting so we
/// never emit a tag Telegram doesn't support (which would 400 the message).
/// What: Strips ANSI escapes, escapes <, >, &, then converts ```lang ... ```
/// into `<pre><code>...</code></pre>`, `code` into `<code>code</code>`, and
/// `**bold**` into `<b>bold</b>`. Anything else passes through as plain text.
/// Test: `markdown_to_html_safe_*` in `telegram::tests`.
pub(super) fn markdown_to_html_safe(input: &str) -> String {
    use teloxide::utils::html;

    // Strip ANSI escapes first — ctrl's output may contain colour codes from
    // tool runners that have no place in Telegram chat.
    let cleaned = strip_ansi(input);
    let escaped = html::escape(&cleaned);

    // Convert fenced code blocks. We use a simple state machine over lines so
    // we don't accidentally rewrite triple-backticks inside user content
    // (the `escaped` step has already turned any user-visible `<` into `&lt;`).
    let mut out = String::with_capacity(escaped.len() + 32);
    let mut in_fence = false;
    for line in escaped.split_inclusive('\n') {
        let trimmed = line.trim_end_matches('\n');
        if trimmed.starts_with("```") {
            if in_fence {
                out.push_str("</code></pre>");
                if line.ends_with('\n') {
                    out.push('\n');
                }
                in_fence = false;
            } else {
                out.push_str("<pre><code>");
                if line.ends_with('\n') {
                    // Drop the language hint line entirely — Telegram <code>
                    // doesn't render syntax classes anyway.
                }
                in_fence = true;
            }
            continue;
        }
        out.push_str(line);
    }
    if in_fence {
        out.push_str("</code></pre>");
    }

    // Inline formatting: inline code FIRST, then bold. Why the reversed
    // order vs. the obvious "bold first"? If we ran bold first, a string
    // like "`foo **bar** baz`" would have its `**` converted to `<b>` *inside*
    // the code span, corrupting it (Telegram <code> renders tags literally,
    // so the user would see "<b>bar</b>" in monospace). Doing code first
    // means `**` markers inside backticks get sealed inside a <code> tag
    // before the bold pass ever sees them, so the bold pass can only
    // affect text outside code spans.
    let coded = convert_pairs(&out, "`", "<code>", "</code>");
    convert_pairs_outside_tag(&coded, "**", "<b>", "</b>", "<code>", "</code>")
}

/// Like `convert_pairs`, but skips ranges that are already inside the named
/// HTML tag (used to prevent bold/italic conversion inside `<code>` spans
/// that we just emitted, which would re-corrupt the code).
///
/// Why: Even with code-first ordering, a code span like "`a` **b** `c`"
/// has plain text outside the spans where bold conversion is desired. But
/// "`**x**`" would already be wrapped — we must not touch tokens inside
/// `<code>…</code>`. Naive `find(delim)` doesn't know about tags. This
/// helper walks the string and toggles a "skip" flag whenever it enters /
/// exits the named tag pair, only running the conversion on outside text.
/// Test: `convert_pairs_outside_tag_skips_code` in `telegram::tests`.
pub(super) fn convert_pairs_outside_tag(
    input: &str,
    delim: &str,
    open: &str,
    close: &str,
    skip_open: &str,
    skip_close: &str,
) -> String {
    let mut out = String::with_capacity(input.len());
    let mut buf = String::new();
    let mut rest = input;
    loop {
        let next_skip = rest.find(skip_open);
        match next_skip {
            None => {
                buf.push_str(rest);
                break;
            }
            Some(idx) => {
                buf.push_str(&rest[..idx]);
                // Flush converted buffer.
                out.push_str(&convert_pairs(&buf, delim, open, close));
                buf.clear();
                // Find the matching close. If none, append the rest as-is
                // (defensive — shouldn't happen since we just emitted these).
                let after_open = &rest[idx + skip_open.len()..];
                match after_open.find(skip_close) {
                    None => {
                        out.push_str(&rest[idx..]);
                        return out;
                    }
                    Some(close_idx) => {
                        let span_end = idx + skip_open.len() + close_idx + skip_close.len();
                        out.push_str(&rest[idx..span_end]);
                        rest = &rest[span_end..];
                    }
                }
            }
        }
    }
    out.push_str(&convert_pairs(&buf, delim, open, close));
    out
}

/// Replace paired `delim` markers with `open`/`close` HTML tags.
///
/// Why: Markdown bold (`**x**`) and inline code (`` `x` ``) are both
/// delimiter-paired. We treat them identically: count occurrences, alternate
/// open/close, leave any unpaired trailing delim as a literal so we don't
/// emit unbalanced HTML (which Telegram would reject).
/// Test: `convert_pairs_alternates_open_close` in `telegram::tests`.
pub(super) fn convert_pairs(input: &str, delim: &str, open: &str, close: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    let mut next_is_open = true;
    while let Some(idx) = rest.find(delim) {
        out.push_str(&rest[..idx]);
        // Lookahead: is there a closing delim later? If not, treat this as
        // literal text.
        let after = &rest[idx + delim.len()..];
        if next_is_open && !after.contains(delim) {
            out.push_str(delim);
            rest = after;
            continue;
        }
        if next_is_open {
            out.push_str(open);
        } else {
            out.push_str(close);
        }
        next_is_open = !next_is_open;
        rest = after;
    }
    out.push_str(rest);
    out
}

/// Strip ANSI escape sequences (CSI / SGR) so terminal colour codes don't
/// leak into Telegram messages.
fn strip_ansi(s: &str) -> String {
    // Reuse the project's existing strip-ansi crate via the `strip_ansi_escapes` dep.
    // `strip_str` returns String in newer versions; the result is a clean
    // String regardless of crate version, so just pass it through.
    strip_ansi_escapes::strip_str(s)
}

/// Last-resort: remove all `<...>` tags and unescape HTML entities so the
/// plain-text fallback shows readable content instead of raw HTML.
///
/// Why: The send-as-HTML path escapes `<`, `>`, `&` to `&lt;`, `&gt;`, `&amp;`
/// before sending. If Telegram rejects the HTML (e.g. unbalanced tags from a
/// bizarre LLM reply), the fallback path used to leak those entities verbatim
/// to the user ("a &lt; b"). We strip tags AND unescape entities here so the
/// fallback message is human-readable.
/// What: First removes `<…>` tags, then replaces `&lt;`, `&gt;`, `&quot;`,
/// `&#39;`, `&amp;` (in this order — `&amp;` must run last to avoid
/// double-decoding).
/// Test: `strip_html_tags_unescapes_entities` in `telegram::tests`.
pub(super) fn strip_html_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    // Unescape entities. Order matters: `&amp;` must come last so we don't
    // turn `&amp;lt;` into `<` (it should stay as `&lt;`).
    out.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}
