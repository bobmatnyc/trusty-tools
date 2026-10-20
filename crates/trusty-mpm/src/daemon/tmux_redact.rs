//! OAuth-token redaction for tmux error text (#2246), split from
//! `daemon/tmux.rs` for the SLOC cap (#8928 step 4).
//!
//! Why/What: see [`redact_oauth_token`].
//! Test: `daemon/tmux_tests.rs` (`redact_oauth_token_*`,
//! `find_quoted_value_end_*`).

use crate::core::oauth_token::OAUTH_TOKEN_ENV_VAR;

/// Find the end (one PAST the true closing `'`, byte offset into `s`) of a
/// POSIX shell single-quoted value, given `s` is the text immediately AFTER
/// the opening quote.
///
/// Why: [`shell_single_quote`](crate::runtime::claude_code) escapes an
/// embedded literal `'` as the 4-byte close-escape-reopen sequence `'\''`
/// (close the current quote, backslash-escape a literal `'`, reopen a new
/// quote) — POSIX shells have no in-quote escape, so this is the standard
/// way to embed a quote inside a single-quoted string. A naive scan for the
/// FIRST `'` after the opener mistakes that sequence's first byte for the
/// closer, truncating the redaction and leaking every byte of the value AFTER
/// the embedded quote in cleartext (a real reviewer-caught edge case: a token
/// containing `'` would leak its tail). This walks the value treating `'\''`
/// as one escaped-quote unit to skip over, so only a BARE `'` — one not
/// followed by `\''` — is treated as the true closer.
/// What: scans byte-by-byte (safe: the only bytes it compares against are `'`
/// and `\`, both single-byte ASCII, so it can never land mid-codepoint of a
/// multi-byte UTF-8 sequence in an untouched run of bytes). At each `'`, if
/// the next three bytes spell `\''` the whole 4-byte run is an escaped
/// literal quote — skipped, continue scanning; otherwise this `'` is the true
/// closer and its index + 1 is returned. Falls back to `s.len()` if no closer
/// is found (malformed input — treat the rest of the string as the value
/// rather than panic or leak).
/// Test: `find_quoted_value_end_stops_at_bare_closer`,
/// `find_quoted_value_end_skips_escaped_embedded_quote`,
/// `redact_oauth_token_masks_value_with_embedded_single_quote`.
pub(super) fn find_quoted_value_end(s: &str) -> usize {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\'' {
            if bytes[i..].starts_with(b"'\\''") {
                i += 4; // escaped embedded quote unit — not the closer
                continue;
            }
            return i + 1; // bare closer — include it in the consumed span
        }
        i += 1;
    }
    bytes.len()
}

/// Mask any `CLAUDE_CODE_OAUTH_TOKEN=<value>` secret in `s` before it reaches a
/// log or a persisted record.
///
/// Why: managed-session pane commands interpolate a cleartext
/// `CLAUDE_CODE_OAUTH_TOKEN='<token>'` (#2246). When [`TmuxDriver::run`] maps a
/// failed `send-keys` to an error it formats the full argv — which would
/// otherwise carry that OAuth secret into every downstream `warn!` log in
/// `daemon::managed_routes::lifecycle` AND into the persisted session `task`
/// field via `session_manager::manager::mark_errored`, where `tm session ls`
/// surfaces it. Redacting at this single source closes every one of those
/// paths at once, so no caller has to remember to scrub.
/// What: replaces the value of each `CLAUDE_CODE_OAUTH_TOKEN=` assignment with
/// `<redacted>`, handling both the single-quoted form
/// (`CLAUDE_CODE_OAUTH_TOKEN='…'` → `CLAUDE_CODE_OAUTH_TOKEN='<redacted>'`,
/// via [`find_quoted_value_end`] so a value containing an escaped embedded
/// quote is masked in full, not just up to its first byte) and a bare
/// unquoted form (value ends at the next ASCII whitespace). All other bytes
/// are copied through unchanged, so a string with no token is returned
/// verbatim.
/// Test: `redact_oauth_token_masks_quoted_value`,
/// `redact_oauth_token_masks_unquoted_value`,
/// `redact_oauth_token_masks_value_with_embedded_single_quote`,
/// `redact_oauth_token_leaves_unrelated_text_untouched`,
/// `run_style_error_message_does_not_leak_oauth_token`.
pub(super) fn redact_oauth_token(s: &str) -> String {
    let needle = format!("{OAUTH_TOKEN_ENV_VAR}=");
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(pos) = rest.find(&needle) {
        out.push_str(&rest[..pos + needle.len()]);
        let after = &rest[pos + needle.len()..];
        if let Some(stripped) = after.strip_prefix('\'') {
            // Single-quoted value: mask up to and including the TRUE closing
            // quote, correctly skipping any escaped embedded quotes.
            let end = find_quoted_value_end(stripped);
            out.push_str("'<redacted>'");
            rest = &stripped[end..];
        } else {
            // Bare value: mask up to the next whitespace (or end of string).
            let end = after.find(char::is_whitespace).unwrap_or(after.len());
            out.push_str("<redacted>");
            rest = &after[end..];
        }
    }
    out.push_str(rest);
    out
}
