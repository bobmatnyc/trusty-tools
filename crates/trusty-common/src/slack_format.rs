//! Shared Slack `mrkdwn` formatting/escaping primitives.
//!
//! Why: the Slack `mrkdwn` escaping and code-fence helpers were born inside
//! trusty-mpm's `slack::formatter` (the inbound `/command` gateway renders
//! `CommandResult`s to `mrkdwn`). A second consumer now exists — the native
//! Slack MCP server in `trusty-channels` (epic #2636, ADR-0014) must apply the
//! identical escaping to untrusted channel/user text so a hostile message
//! cannot inject markup (e.g. a `<!channel>` broadcast span) into rendered
//! output. Re-implementing the escape rule in a second crate would risk the two
//! diverging on the exact substitution set, so the reusable, domain-free
//! primitives live here (the crate both already depend on) as the single source
//! of truth. The `CommandResult`-rendering `SlackFormatter` itself stays in
//! trusty-mpm because it is coupled to trusty-mpm's domain types; only these
//! pure helpers are shared.
//! What: [`mrkdwn_escape`] neutralises the three `mrkdwn`-significant characters;
//! [`code_block`] wraps text in a triple-backtick fence; [`code_inline`] wraps a
//! span in single backticks. All are pure `std` string operations — no
//! dependencies, no feature gate.
//! Test: the unit tests in this module cover each helper; trusty-mpm's
//! `slack::formatter::tests` and trusty-channels' handler tests exercise them
//! through their re-exports.

/// Escape the three `mrkdwn`-significant characters for a Slack message body.
///
/// Why: any backend- or user-controlled text interpolated into a Slack
/// `mrkdwn` reply (a session name, a channel message read back from Slack, an
/// error string) can carry Slack's special `<...>` link/mention/broadcast
/// markup. `<!channel>` broadcast-pings the whole channel and `<@U123>` mentions
/// a user; an unescaped message containing `<!channel>` would broadcast-ping the
/// channel from every rendered reply. Escaping the delimiters neutralises the
/// injection while leaving the visible text intact.
/// What: replaces `&` → `&amp;`, `<` → `&lt;`, `>` → `&gt;`, in that order (so
/// the `&` produced by the `<`/`>` substitutions is never re-escaped). Slack
/// unescapes exactly these three entities in `mrkdwn` bodies, matching Slack's
/// own documented escaping contract for `chat.postMessage`.
/// Test: `mrkdwn_escape_escapes_ampersand_lt_gt`,
/// `mrkdwn_escape_neutralizes_channel_broadcast_span`.
pub fn mrkdwn_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Wrap text in a Slack triple-backtick code fence.
///
/// Why: pane output and captured command/message output should render
/// monospaced in Slack; `mrkdwn` uses triple backticks for that.
/// What: returns ```` ```\n<text>\n``` ````.
/// Test: `code_block_wraps_in_triple_backticks`.
pub fn code_block(text: &str) -> String {
    format!("```\n{text}\n```")
}

/// Wrap a short span in a single-backtick inline-code run.
///
/// Why: ids, channel names, and other literal tokens read best monospaced
/// inline rather than as a full fenced block.
/// What: returns `` `<s>` ``. The caller is responsible for ensuring the span
/// contains no backticks (ids/names never do).
/// Test: `code_inline_wraps_in_single_backticks`.
pub fn code_inline(s: &str) -> String {
    format!("`{s}`")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mrkdwn_escape_escapes_ampersand_lt_gt() {
        assert_eq!(mrkdwn_escape("a & b"), "a &amp; b");
        assert_eq!(mrkdwn_escape("<tag>"), "&lt;tag&gt;");
        assert_eq!(mrkdwn_escape("a < b & c > d"), "a &lt; b &amp; c &gt; d");
        // plain text is unchanged
        assert_eq!(mrkdwn_escape("plain text"), "plain text");
    }

    #[test]
    fn mrkdwn_escape_neutralizes_channel_broadcast_span() {
        // A hostile message name must not survive as a live broadcast span.
        let hostile = "<!channel> everybody";
        let escaped = mrkdwn_escape(hostile);
        assert!(!escaped.contains('<'), "no raw < survives");
        assert!(!escaped.contains('>'), "no raw > survives");
        assert_eq!(escaped, "&lt;!channel&gt; everybody");
    }

    #[test]
    fn mrkdwn_escape_does_not_double_escape_ampersand() {
        // The `&` from the <-substitution must not be re-escaped into `&amp;lt;`.
        assert_eq!(mrkdwn_escape("<"), "&lt;");
        assert_eq!(mrkdwn_escape(">"), "&gt;");
    }

    #[test]
    fn code_block_wraps_in_triple_backticks() {
        assert_eq!(code_block("hello"), "```\nhello\n```");
    }

    #[test]
    fn code_inline_wraps_in_single_backticks() {
        assert_eq!(code_inline("C123"), "`C123`");
    }
}
