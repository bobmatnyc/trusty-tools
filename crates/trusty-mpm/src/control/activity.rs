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
/// Why: git commit/push are high-value signals for the SM agent.
/// What: matches common git output lines containing commit/push/pull/merge verbs.
/// Test: `activity_parser_git_op`.
fn re_git_op() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?xi)
            (?:
                \[(?P<branch>\S+)\s+\w+\]\s+(?P<msg>.{1,80})  # git commit: [branch abc] msg
                |
                (?P<verb>push(?:ing|ed)?|pull(?:ing|ed)?|merg(?:ing|ed)?|fetch(?:ing|ed)?)
                  (?:\s+to|\s+from|\s+origin|\s+upstream)?
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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Table-driven: (input, expected_kind, summary_contains, pending_decision, proposed_default)
    struct Case {
        input: &'static str,
        expected_kind: ActivityKind,
        summary_prefix: &'static str,
        pending_decision: Option<&'static str>,
        proposed_default: Option<&'static str>,
    }

    #[test]
    fn activity_parser_awaiting_input() {
        let result = ActivityParser::parse_output("> ");
        assert!(result.is_some(), "should match awaiting input");
        let r = result.unwrap();
        assert_eq!(r.kind, ActivityKind::AwaitingInput);
        assert!(r.summary.contains("awaiting"));
    }

    #[test]
    fn activity_parser_tool_use() {
        let result = ActivityParser::parse_output("Tool use: Bash");
        assert!(result.is_some());
        let r = result.unwrap();
        assert_eq!(
            r.kind,
            ActivityKind::ToolUse {
                name: "Bash".into()
            }
        );
        assert!(r.summary.contains("Bash"));
    }

    #[test]
    fn activity_parser_tool_use_using_tool() {
        let result = ActivityParser::parse_output("Using tool: Edit");
        assert!(result.is_some());
        let r = result.unwrap();
        assert_eq!(
            r.kind,
            ActivityKind::ToolUse {
                name: "Edit".into()
            }
        );
    }

    #[test]
    fn activity_parser_error() {
        let result = ActivityParser::parse_output("Error: file not found");
        assert!(result.is_some());
        let r = result.unwrap();
        assert!(matches!(r.kind, ActivityKind::Error { .. }));
        assert!(r.summary.contains("error"));
    }

    #[test]
    fn activity_parser_test_result_cargo() {
        let result =
            ActivityParser::parse_output("test result: ok. 42 passed; 0 failed; 0 ignored");
        assert!(result.is_some());
        let r = result.unwrap();
        assert_eq!(
            r.kind,
            ActivityKind::TestResult {
                passed: 42,
                failed: 0
            }
        );
        assert!(r.summary.contains("42 passed"));
    }

    #[test]
    fn activity_parser_test_result_cargo_failed() {
        let result =
            ActivityParser::parse_output("test result: FAILED. 10 passed; 3 failed; 0 ignored");
        assert!(result.is_some());
        let r = result.unwrap();
        assert_eq!(
            r.kind,
            ActivityKind::TestResult {
                passed: 10,
                failed: 3
            }
        );
        assert!(r.summary.contains("3 failed"));
    }

    #[test]
    fn activity_parser_test_result_pytest() {
        let result = ActivityParser::parse_output("5 passed, 2 failed");
        assert!(result.is_some());
        let r = result.unwrap();
        assert_eq!(
            r.kind,
            ActivityKind::TestResult {
                passed: 5,
                failed: 2
            }
        );
    }

    #[test]
    fn activity_parser_git_op() {
        let cases = [
            "git commit - [main abc1234] Add feature",
            "pushing to origin",
            "Merging branch 'feature' into main",
        ];
        for input in &cases {
            let result = ActivityParser::parse_output(input);
            assert!(result.is_some(), "expected match for: {input}");
            let r = result.unwrap();
            assert!(
                matches!(r.kind, ActivityKind::GitOp { .. }),
                "input={input}"
            );
        }
    }

    #[test]
    fn activity_parser_auth_prompt() {
        let cases = [
            "Please log in to claude.ai",
            "Run claude auth login",
            "Authentication required",
        ];
        for input in &cases {
            let result = ActivityParser::parse_output(input);
            assert!(result.is_some(), "expected match for: {input}");
            let r = result.unwrap();
            assert_eq!(r.kind, ActivityKind::AuthPrompt, "input={input}");
        }
    }

    #[test]
    fn activity_parser_session_complete() {
        let result = ActivityParser::parse_output("Session complete");
        assert!(result.is_some());
        let r = result.unwrap();
        assert_eq!(r.kind, ActivityKind::SessionComplete);
    }

    #[test]
    fn activity_parser_rate_limited() {
        let cases = ["rate limit exceeded", "429 Too Many Requests"];
        for input in &cases {
            let result = ActivityParser::parse_output(input);
            assert!(result.is_some(), "expected match for: {input}");
            let r = result.unwrap();
            assert_eq!(r.kind, ActivityKind::RateLimited, "input={input}");
        }
    }

    #[test]
    fn activity_parser_pending_decision_yes_default() {
        let result =
            ActivityParser::parse_output("Do you want to proceed with the file deletion? [Y/n]:");
        assert!(result.is_some());
        let r = result.unwrap();
        assert_eq!(r.kind, ActivityKind::AwaitingInput);
        assert!(r.pending_decision.is_some());
        assert_eq!(r.proposed_default.as_deref(), Some("yes"));
    }

    #[test]
    fn activity_parser_pending_decision_no_default() {
        let result = ActivityParser::parse_output("Continue with destructive operation? [y/N]:");
        assert!(result.is_some());
        let r = result.unwrap();
        assert_eq!(r.kind, ActivityKind::AwaitingInput);
        assert!(r.pending_decision.is_some());
        assert_eq!(r.proposed_default.as_deref(), Some("no"));
    }

    #[test]
    fn activity_parser_returns_none_for_unknown_output() {
        let result = ActivityParser::parse_output(
            "Some totally normal progress message without any keywords",
        );
        // Should not match any pattern.
        assert!(
            result.is_none(),
            "should not match unrecognised output: got {result:?}"
        );
    }

    /// Table-driven summary of all cases — kept as a supplementary sweep.
    #[test]
    fn activity_parser_table_driven() {
        let cases: Vec<Case> = vec![
            Case {
                input: "> ",
                expected_kind: ActivityKind::AwaitingInput,
                summary_prefix: "awaiting",
                pending_decision: None,
                proposed_default: None,
            },
            Case {
                input: "Tool use: Bash",
                expected_kind: ActivityKind::ToolUse {
                    name: "Bash".into(),
                },
                summary_prefix: "tool use",
                pending_decision: None,
                proposed_default: None,
            },
            Case {
                input: "Error: cannot read file",
                expected_kind: ActivityKind::Error {
                    excerpt: "cannot read file".into(),
                },
                summary_prefix: "error",
                pending_decision: None,
                proposed_default: None,
            },
            Case {
                input: "test result: ok. 7 passed; 0 failed; 0 ignored",
                expected_kind: ActivityKind::TestResult {
                    passed: 7,
                    failed: 0,
                },
                summary_prefix: "tests:",
                pending_decision: None,
                proposed_default: None,
            },
            Case {
                input: "Session complete",
                expected_kind: ActivityKind::SessionComplete,
                summary_prefix: "session complete",
                pending_decision: None,
                proposed_default: None,
            },
            Case {
                input: "Please log in to use Claude",
                expected_kind: ActivityKind::AuthPrompt,
                summary_prefix: "authentication",
                pending_decision: Some("Please log in to use Claude"),
                proposed_default: None,
            },
            Case {
                input: "rate limit exceeded: try again later",
                expected_kind: ActivityKind::RateLimited,
                summary_prefix: "rate-limit",
                pending_decision: None,
                proposed_default: None,
            },
        ];

        for c in &cases {
            let result = ActivityParser::parse_output(c.input);
            assert!(result.is_some(), "expected Some for input={:?}", c.input);
            let r = result.unwrap();
            // For ToolUse and Error, compare only the variant discriminant.
            let kind_matches = match (&r.kind, &c.expected_kind) {
                (ActivityKind::ToolUse { name: a }, ActivityKind::ToolUse { name: b }) => a == b,
                (ActivityKind::Error { .. }, ActivityKind::Error { .. }) => true,
                (a, b) => a == b,
            };
            assert!(
                kind_matches,
                "kind mismatch for input={:?}: got {:?}, want {:?}",
                c.input, r.kind, c.expected_kind
            );
            assert!(
                r.summary.contains(c.summary_prefix),
                "summary {:?} missing prefix {:?} for input={:?}",
                r.summary,
                c.summary_prefix,
                c.input
            );
            assert_eq!(
                r.pending_decision.as_deref(),
                c.pending_decision,
                "pending_decision mismatch for input={:?}",
                c.input
            );
            assert_eq!(
                r.proposed_default.as_deref(),
                c.proposed_default,
                "proposed_default mismatch for input={:?}",
                c.input
            );
        }
    }
}
