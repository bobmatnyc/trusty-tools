//! The shape of an LLM reply that failed to parse, as one bounded line (#9310).
//!
//! Why: 37 of 80 Sonnet 5.5 reviews in the Q86 model eval ended "review not
//! parsed", and the raw replies were not kept, so the failing shape is
//! unknown. The parse-failure record must say what the reply looked like
//! without carrying it whole.
//! What: [`describe_reply`] renders the stop reason, output tokens, text length,
//! and a short head and tail of the text with credential-shaped tokens masked.
//! The Converse content-block kinds are not in an `LlmResponse`; the Bedrock
//! provider logs them itself (`tool_use::reply_block_kinds`).
//! Test: `reply_shape_tests.rs`.

use crate::llm::LlmResponse;

/// Characters kept from each end of the reply text.
const SNIPPET_CHARS: usize = 80;

/// A credential-alphabet run at least this long, with a letter and a digit,
/// is masked.
const MASK_MIN_CHARS: usize = 20;

/// One line describing `resp` for a parse-failure log and error string.
///
/// Why: the model-eval harness row carries only `ReviewResult::error`, so the
/// shape must fit in that string (#9310).
/// What: `reply shape: stop=<reason|none> output_tokens=<n> text_chars=<n>
/// head=<quoted> tail=<quoted>`. Head and tail are at most [`SNIPPET_CHARS`]
/// characters each, Debug-quoted so the line has no raw newline; a text of at
/// most twice that length is all head and an empty tail. Masking runs over the
/// whole text before the cut, so no secret is split into an unmasked fragment.
/// The result is lower-risk, not proven secret-free.
/// Test: `describe_reply_carries_stop_tokens_and_bounded_snippets`,
/// `describe_reply_masks_credential_shaped_tokens`,
/// `describe_reply_short_text_is_all_head`.
pub(crate) fn describe_reply(resp: &LlmResponse) -> String {
    let masked = mask_credential_shapes(resp.text.trim());
    let chars = masked.chars().count();
    let (head, tail) = if chars <= SNIPPET_CHARS * 2 {
        (masked, String::new())
    } else {
        let head: String = masked.chars().take(SNIPPET_CHARS).collect();
        let tail: String = masked.chars().skip(chars - SNIPPET_CHARS).collect();
        (head, tail)
    };
    format!(
        "reply shape: stop={} output_tokens={} text_chars={} head={head:?} tail={tail:?}",
        resp.finish_reason.as_deref().unwrap_or("none"),
        resp.output_tokens,
        resp.text.trim().chars().count(),
    )
}

/// Characters a machine-generated credential is drawn from.
fn is_credential_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '_' | '-')
}

/// Replace every credential-shaped run in `text` with `[masked N chars]`.
///
/// What: a run of [`is_credential_char`] characters at least
/// [`MASK_MIN_CHARS`] long holding both a letter and a digit. A plain
/// identifier or path with no digit is kept, so the snippet stays readable.
/// Test: `describe_reply_masks_credential_shaped_tokens`.
fn mask_credential_shapes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut run = String::new();
    for c in text.chars() {
        if is_credential_char(c) {
            run.push(c);
            continue;
        }
        flush_run(&mut out, &mut run);
        out.push(c);
    }
    flush_run(&mut out, &mut run);
    out
}

/// Append `run` to `out`, masked when it is credential-shaped, and clear it.
fn flush_run(out: &mut String, run: &mut String) {
    let credential = run.len() >= MASK_MIN_CHARS
        && run.bytes().any(|b| b.is_ascii_digit())
        && run.bytes().any(|b| b.is_ascii_alphabetic());
    if credential {
        out.push_str(&format!("[masked {} chars]", run.len()));
    } else {
        out.push_str(run);
    }
    run.clear();
}

#[cfg(test)]
#[path = "reply_shape_tests.rs"]
mod tests;
