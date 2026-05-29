//! Message chunking + Markdown→mrkdwn conversion for the Slack gateway.
//!
//! Why: ctrl replies are standard Markdown and may exceed Slack's recommended
//! per-message length; these helpers split on newline boundaries and rewrite
//! `**bold**` into Slack's single-asterisk `*bold*` form.
//! What: `split_message`, `markdown_to_mrkdwn`, the asterisk converter, and the
//! ANSI stripper, plus the `MAX_SLACK_MESSAGE` chunk size.
//! Test: `split_message_*`, `markdown_to_mrkdwn_*`,
//! `convert_double_to_single_asterisk_*` in `slack::tests`.

/// Maximum characters per Slack message block.
///
/// Why: Slack's `chat.postMessage` `text` field has a 40k limit overall, but
/// individual mrkdwn blocks are recommended to stay <= 3000 chars for
/// readability and reliable rendering. Long ctrl responses are split at
/// newline boundaries before this limit.
pub(super) const MAX_SLACK_MESSAGE: usize = 3000;

/// Split `text` into chunks of at most `max_len` chars, preferring to break
/// on newlines.
///
/// Why: Slack mrkdwn renders best when individual messages stay <= 3000
/// chars. Hard-splitting mid-line yields ugly output; we prefer the
/// rightmost newline in the first `max_len` chars, falling back to a hard
/// (UTF-8-safe) split when no newline is available.
/// What: Returns a `Vec<String>` whose concatenation equals `text`.
/// Test: `split_message_*` in `slack::tests`.
pub(super) fn split_message(text: &str, max_len: usize) -> Vec<String> {
    if text.len() <= max_len {
        return vec![text.to_owned()];
    }
    let mut chunks = Vec::new();
    let mut remaining = text;
    while remaining.len() > max_len {
        let mut boundary = max_len;
        while boundary > 0 && !remaining.is_char_boundary(boundary) {
            boundary -= 1;
        }
        if boundary == 0 {
            chunks.push(remaining.to_owned());
            return chunks;
        }
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

/// Convert ctrl's Markdown-ish output into Slack mrkdwn.
///
/// Why: ctrl emits standard Markdown (`**bold**`, `` `code` ``, ``` ``` ```
/// fences). Slack mrkdwn uses `*bold*` (single asterisk) and preserves
/// triple-backtick fences and single-backtick inline code as-is. We strip
/// ANSI escapes and rewrite `**x**` -> `*x*` while leaving code spans
/// untouched.
/// Test: `markdown_to_mrkdwn_*` in `slack::tests`.
pub(super) fn markdown_to_mrkdwn(input: &str) -> String {
    let cleaned = strip_ansi(input);
    // Convert **bold** -> *bold*. Order matters: do this before any other
    // asterisk-touching rewrite. We use a paired-delimiter walker so
    // unbalanced `**` passes through as literal.
    convert_double_to_single_asterisk(&cleaned)
}

/// Replace paired `**` delimiters with single `*` for Slack mrkdwn bold.
///
/// Why: Slack mrkdwn uses `*bold*`, not Markdown's `**bold**`. Unbalanced
/// `**` is left as-is so we don't corrupt arbitrary asterisk content.
/// Test: `convert_double_to_single_asterisk_*` in `slack::tests`.
pub(super) fn convert_double_to_single_asterisk(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    let mut next_is_open = true;
    while let Some(idx) = rest.find("**") {
        out.push_str(&rest[..idx]);
        let after = &rest[idx + 2..];
        if next_is_open && !after.contains("**") {
            // Unpaired — emit literal.
            out.push_str("**");
            rest = after;
            continue;
        }
        out.push('*');
        next_is_open = !next_is_open;
        rest = after;
    }
    out.push_str(rest);
    out
}

/// Strip ANSI escape sequences (CSI / SGR) so terminal colour codes don't
/// leak into Slack messages.
fn strip_ansi(s: &str) -> String {
    strip_ansi_escapes::strip_str(s)
}
