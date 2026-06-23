//! Regex/NLP parse layer for session output (§8.2 of SPEC-SESSCTL-01, WI-3 #1594).
//!
//! Why: raw session output bytes must be classified into structured
//! `ActivityKind` labels WITHOUT any LLM call so the daemon can update
//! `SessionMetadata.last_summary` and emit `ActivityParsed`/`PendingDecision`
//! events with zero network latency.
//! What: [`ActivityParser`] owns compiled `Regex` patterns (via `OnceLock`)
//! and exposes `parse_output`, which returns an optional [`ParseResult`]
//! carrying the matched kind, a short human summary, and optional
//! pending-decision fields.
//! Test: `activity_parser_*` table-driven tests in the inline test module.

use std::sync::OnceLock;

use regex::Regex;

use crate::control::event::ActivityKind;

// ── Compiled regex statics ────────────────────────────────────────────────────

/// Returns the regex for Claude's interactive `> ` prompt.
///
/// Why: the prompt is the primary signal that Claude is awaiting user input.
/// What: matches lines starting with `> ` or the standalone pattern.
/// Test: `activity_parser_awaiting_input`.
fn re_awaiting_input() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?m)^\s*>\s").expect("awaiting_input regex"))
}

/// Returns the regex for tool-use lines.
///
/// Why: observers want to know when Claude is invoking a tool.
/// What: matches `Tool use: <name>` or `Using tool: <name>`.
/// Test: `activity_parser_tool_use`.
fn re_tool_use() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)(?:Tool use|Using tool):\s*(?P<name>\S+)").expect("tool_use regex")
    })
}

/// Returns the regex for error lines.
///
/// Why: surface error conditions to observers without waiting for an LLM pass.
/// What: matches lines starting with `Error:` or containing ANSI error escape.
/// Test: `activity_parser_error`.
fn re_error() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?m)(?:(?:^|\n)\s*Error:\s*(?P<msg>[^\n]{1,120})|error\[E\d+\])")
            .expect("error regex")
    })
}

/// Returns the regex for cargo/pytest test result lines.
///
/// Why: test results are high-signal events for the SM agent and operators.
/// What: matches `test result: ok. N passed; M failed` or
/// `M tests passed` / `M passed, N failed`.
/// Test: `activity_parser_test_result`.
fn re_test_result() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?xi)
            # cargo test: `test result: ok. N passed; M failed`
            test\ result:\ (?:ok|FAILED)\.\ \s*
            (?P<p1>\d+)\ passed;\ (?P<f1>\d+)\ failed
            |
            # pytest style: `N passed` or `N passed, M failed`
            (?P<p2>\d+)\ passed(?:,\s*(?P<f2>\d+)\ failed)?
            |
            # also match `N tests passed`
            (?P<p3>\d+)\ tests?\ passed
            ",
        )
        .expect("test_result regex")
    })
}

/// Returns the regex for git operation confirmations.
///
/// Why: git commit/push are high-value signals for the SM agent. Word-boundary
/// anchors (`\b`) and an optional `git ` prefix prevent false matches on prose
/// phrases like "fetching dependencies" or "merging results from database".
/// What: matches `[branch abc] commit message` lines (git commit output) OR
/// git verb lines that start with `git ` / appear at the line start with a
/// \b boundary so only standalone git vocabulary triggers a match.
/// Test: `activity_parser_git_op`, `activity_parser_git_op_no_false_positives`.
fn re_git_op() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?xi)
            (?:
                # git commit output: [branch abc1234] message
                \[(?P<branch>\S+)\s+[0-9a-f]+\]\s+(?P<msg>.{1,80})
                |
                # explicit `git <verb>` invocation
                \bgit\s+(?P<verb>push|pull|merge|fetch|commit|rebase|clone)(?:ing|ed)?\b
            )",
        )
        .expect("git_op regex")
    })
}

/// Returns the regex for OAuth / Max login prompts.
///
/// Why: §4.4 and §8.2 require `AuthPrompt` detection for both backends.
/// What: matches known Claude auth/login prompt phrases.
/// Test: `activity_parser_auth_prompt`.
fn re_auth_prompt() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)(?:please\s+log\s*in|claude\.ai.*login|oauth|log\s*in\s+to\s+claude|authentication\s+required|run\s+claude\s+auth\s+login)",
        )
        .expect("auth_prompt regex")
    })
}

/// Returns the regex for session-complete signals.
///
/// Why: clean exit from a session should be surfaced to observers immediately.
/// What: matches `Session complete` or `session complete` phrases.
/// Test: `activity_parser_session_complete`.
fn re_session_complete() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)session\s+complete").expect("session_complete regex"))
}

/// Returns the regex for rate-limit errors (§9.2).
///
/// Why: rate-limit events must be surfaced so the SM agent can back off.
/// What: matches known rate-limit error phrases from the Claude API.
/// Test: `activity_parser_rate_limited`.
fn re_rate_limited() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)(?:rate.?limit|too\s+many\s+requests|429)").expect("rate_limited regex")
    })
}

/// Returns the regex for decision prompts with an optional `[Y/n]` default.
///
/// Why: §8.4 — when the output contains a `[Y/n]`-style prompt the daemon
/// must extract both the prompt text and the proposed default.
/// What: matches `… [Y/n]`, `… (yes/no)`, or generic `?` question endings.
/// Test: `activity_parser_pending_decision`.
fn re_pending_decision() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?x)
            (?P<question>[^\n]{10,200}?)         # question text (non-greedy)
            \s*
            (?:
                \[(?P<yes>[Yy])/(?P<no>[Nn])\]   # [Y/n] or [y/N]
                |
                \((?:yes|no)/(?:yes|no)\)         # (yes/no)
            )
            \s*:?\s*$                             # optional colon + line end
            ",
        )
        .expect("pending_decision regex")
    })
}

// ── Result type ───────────────────────────────────────────────────────────────

/// Output of a successful `ActivityParser::parse_output` call.
///
/// Why: callers (the actor loop) need all three fields to update metadata and
/// emit the right events without re-running the parse.
/// What: carries the classified kind, a human-readable summary, and optional
/// pending-decision fields extracted from the output.
/// Test: `activity_parser_*` table-driven tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseResult {
    /// The classified activity kind.
    pub kind: ActivityKind,
    /// Short human-readable summary (<= 200 chars).
    pub summary: String,
    /// If the output is asking the user a question, the prompt text.
    pub pending_decision: Option<String>,
    /// If a `[Y/n]`-style default is present, the proposed answer.
    pub proposed_default: Option<String>,
}

// ── Parser ────────────────────────────────────────────────────────────────────

/// Zero-state regex/NLP parser for raw session output (§8.2 of SPEC-SESSCTL-01).
///
/// Why: having a struct (even with no fields) gives us a clear ownership
/// boundary and a natural extension point for configuration (e.g. custom
/// patterns per session type).
/// What: `parse_output` runs the pattern chain over `text` in priority order
/// and returns `Some(ParseResult)` on the first match, `None` when the text
/// does not match any known pattern.
/// Test: `activity_parser_returns_none_for_unknown_output`.
pub struct ActivityParser;

impl ActivityParser {
    /// Run the regex parse chain over `text`.
    ///
    /// Why: the actor calls this after every `Output` event; returning `None`
    /// on no-match avoids creating a spurious `ActivityParsed` event.
    /// What: tries each pattern in priority order (auth → rate-limit → tool-use →
    /// error → test → git → session-complete → decision → awaiting-input) and
    /// returns on the first match. Allocation only occurs when a pattern matches.
    /// Test: `activity_parser_*` table-driven tests.
    pub fn parse_output(text: &str) -> Option<ParseResult> {
        // 1. Auth prompt — highest priority; terminal risk.
        if re_auth_prompt().is_match(text) {
            return Some(ParseResult {
                kind: ActivityKind::AuthPrompt,
                summary: "authentication / login prompt detected".into(),
                pending_decision: Some(text.lines().next().unwrap_or(text).trim().to_owned()),
                proposed_default: None,
            });
        }

        // 2. Rate-limit errors (§9.2).
        if re_rate_limited().is_match(text) {
            return Some(ParseResult {
                kind: ActivityKind::RateLimited,
                summary: "rate-limit error detected".into(),
                pending_decision: None,
                proposed_default: None,
            });
        }

        // 3. Session complete.
        if re_session_complete().is_match(text) {
            return Some(ParseResult {
                kind: ActivityKind::SessionComplete,
                summary: "session completed normally".into(),
                pending_decision: None,
                proposed_default: None,
            });
        }

        // 4. Tool use.
        if let Some(caps) = re_tool_use().captures(text) {
            let name = caps
                .name("name")
                .map_or("unknown", |m| m.as_str())
                .to_owned();
            let summary = format!("tool use: {name}");
            return Some(ParseResult {
                kind: ActivityKind::ToolUse { name },
                summary,
                pending_decision: None,
                proposed_default: None,
            });
        }

        // 5. Error.
        if let Some(caps) = re_error().captures(text) {
            let excerpt = caps
                .name("msg")
                .map_or_else(
                    || text.lines().next().unwrap_or("error").trim(),
                    |m| m.as_str(),
                )
                .chars()
                .take(100)
                .collect::<String>();
            let summary = format!("error: {excerpt}");
            return Some(ParseResult {
                kind: ActivityKind::Error { excerpt },
                summary,
                pending_decision: None,
                proposed_default: None,
            });
        }

        // 6. Test result.
        if let Some(caps) = re_test_result().captures(text) {
            let passed = caps
                .name("p1")
                .or_else(|| caps.name("p2"))
                .or_else(|| caps.name("p3"))
                .and_then(|m| m.as_str().parse::<u32>().ok())
                .unwrap_or(0);
            let failed = caps
                .name("f1")
                .or_else(|| caps.name("f2"))
                .and_then(|m| m.as_str().parse::<u32>().ok())
                .unwrap_or(0);
            let summary = format!("tests: {passed} passed, {failed} failed");
            return Some(ParseResult {
                kind: ActivityKind::TestResult { passed, failed },
                summary,
                pending_decision: None,
                proposed_default: None,
            });
        }

        // 7. Git operation.
        if let Some(caps) = re_git_op().captures(text) {
            let verb = caps
                .name("verb")
                .or_else(|| caps.name("branch"))
                .map_or("commit", |m| m.as_str())
                .to_owned();
            let summary = format!("git: {verb}");
            return Some(ParseResult {
                kind: ActivityKind::GitOp { verb },
                summary,
                pending_decision: None,
                proposed_default: None,
            });
        }

        // 8. Pending decision prompt (§8.4) — before AwaitingInput so `[Y/n]`
        //    prompts are classified as PendingDecision, not generic AwaitingInput.
        if let Some(caps) = re_pending_decision().captures(text) {
            let question = caps
                .name("question")
                .map_or(text, |m| m.as_str())
                .trim()
                .to_owned();
            // Convention: in `[Y/n]` the uppercase letter is the default answer.
            // `Y` (uppercase) → proposed default is "yes"; `y` (lowercase) →
            // proposed default is "no" (meaning `N` is the uppercase default).
            let proposed_default = caps.name("yes").map(|m| {
                if m.as_str().chars().next().is_some_and(|c| c.is_uppercase()) {
                    "yes".to_owned()
                } else {
                    "no".to_owned()
                }
            });
            let summary = format!("awaiting decision: {question}");
            return Some(ParseResult {
                kind: ActivityKind::AwaitingInput,
                summary,
                pending_decision: Some(question),
                proposed_default,
            });
        }

        // 9. Generic awaiting-input (Claude `> ` prompt).
        if re_awaiting_input().is_match(text) {
            return Some(ParseResult {
                kind: ActivityKind::AwaitingInput,
                summary: "awaiting input".into(),
                pending_decision: None,
                proposed_default: None,
            });
        }

        None
    }
}

#[cfg(test)]
#[path = "activity_tests.rs"]
mod tests;
