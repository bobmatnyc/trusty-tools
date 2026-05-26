//! Rule-based, zero-latency session overseer.
//!
//! Why: oversight must be available even when no LLM is reachable; a
//! deterministic, rule-driven overseer gives the daemon a dependency-free
//! default that is fast enough to sit on the hook hot path.
//! What: [`DeterministicOverseer`] implements [`Overseer`] using the
//! [`OverseerConfig`] blocklist / auto-approve substrings, a per-session
//! sliding-window rate limiter, and the question → response auto-responder.
//! Test: `cargo test -p trusty-mpm-core deterministic_overseer` covers
//! blocklist/auto-approve, the rate limiter, and the auto-responder.

use std::collections::{HashMap, VecDeque};
use std::sync::RwLock;
use std::time::{Duration, Instant};

use crate::core::overseer::{Overseer, OverseerContext, OverseerDecision};
use crate::core::overseer_config::OverseerConfig;
use crate::core::session::SessionId;

/// Sliding-window length for the per-session tool-call rate limiter.
const RATE_WINDOW: Duration = Duration::from_secs(60);

/// Rule-based [`Overseer`] driven entirely by an [`OverseerConfig`].
///
/// Why: the daemon needs an always-available oversight strategy with no
/// network calls; this type encapsulates the rules and the rate-limit state.
/// What: holds the immutable policy plus a `RwLock`-guarded map of recent
/// tool-call timestamps per session for sliding-window rate limiting.
/// Test: `blocks_blocklisted_input`, `allows_auto_approved_input`,
/// `rate_limiter_blocks_after_limit`, `auto_responder_matches`.
#[derive(Debug)]
pub struct DeterministicOverseer {
    /// The loaded overseer policy.
    config: OverseerConfig,
    /// Per-session sliding window of recent tool-call instants.
    ///
    /// Why: rate limiting is per session; an `Instant` deque per session lets
    /// the window be pruned in O(expired) on each call.
    /// What: `RwLock<HashMap<...>>` — written on every `pre_tool_use`, so a
    /// write lock is acquired there; reads never race the writes.
    rate: RwLock<HashMap<SessionId, VecDeque<Instant>>>,
}

impl DeterministicOverseer {
    /// Build an overseer from a loaded policy.
    ///
    /// Why: the daemon loads `overseer.toml` once at startup and hands the
    /// resulting config here; the overseer owns it for its lifetime.
    /// What: stores the config and an empty rate-limit map.
    /// Test: `disabled_overseer_allows_everything`.
    pub fn new(config: OverseerConfig) -> Self {
        Self {
            config,
            rate: RwLock::new(HashMap::new()),
        }
    }

    /// Read-only view of the loaded policy.
    ///
    /// Why: the `GET /overseer` endpoint surfaces the active config.
    /// What: returns a reference to the stored [`OverseerConfig`].
    /// Test: `config_accessor_returns_policy`.
    pub fn config(&self) -> &OverseerConfig {
        &self.config
    }

    /// Record a tool call for `session` and report whether it is within budget.
    ///
    /// Why: rate limiting must be a single atomic "prune-window, push, count"
    /// so concurrent hook events cannot race past the limit.
    /// What: drops timestamps older than [`RATE_WINDOW`], pushes `now`, then
    /// returns `true` when the window size is within
    /// `max_tool_calls_per_minute`.
    /// Test: `rate_limiter_blocks_after_limit`.
    fn record_and_check_rate(&self, session: SessionId, now: Instant) -> bool {
        let limit = self.config.deterministic.max_tool_calls_per_minute as usize;
        let mut map = self.rate.write().expect("rate lock not poisoned");
        let window = map.entry(session).or_default();
        let cutoff = now.checked_sub(RATE_WINDOW);
        while let Some(&front) = window.front() {
            match cutoff {
                Some(cutoff) if front < cutoff => {
                    window.pop_front();
                }
                _ => break,
            }
        }
        window.push_back(now);
        window.len() <= limit
    }
}

/// Case-insensitive substring test.
fn contains_ci(haystack: &str, needle: &str) -> bool {
    haystack
        .to_ascii_lowercase()
        .contains(&needle.to_ascii_lowercase())
}

impl Overseer for DeterministicOverseer {
    /// Evaluate a tool invocation before it runs.
    ///
    /// Why: the daemon gates dangerous tool calls (e.g. `rm -rf /`), fast-paths
    /// known-safe ones, and caps runaway tool-call rates — all before the tool
    /// executes.
    /// What: when disabled, returns `Allow` immediately. Otherwise: a blocklist
    /// substring match blocks; an auto-approve substring match allows; an
    /// exceeded rate limit blocks; the default is `Allow`.
    /// Test: `blocks_blocklisted_input`, `allows_auto_approved_input`,
    /// `rate_limiter_blocks_after_limit`, `disabled_overseer_allows_everything`.
    fn pre_tool_use(&self, ctx: &OverseerContext) -> OverseerDecision {
        if !self.config.enabled {
            return OverseerDecision::Allow;
        }
        let input = ctx.tool_input.as_deref().unwrap_or("");

        // 1. Blocklist — substring match blocks outright.
        for pattern in &self.config.deterministic.blocklist {
            if input.contains(pattern.as_str()) {
                return OverseerDecision::Block {
                    reason: format!("tool input matched blocklist entry '{pattern}'"),
                };
            }
        }

        // 2. Auto-approve — substring match short-circuits to Allow.
        for pattern in &self.config.deterministic.auto_approve {
            if input.contains(pattern.as_str()) {
                return OverseerDecision::Allow;
            }
        }

        // 3. Rate limit — too many tool calls in the sliding window blocks.
        if !self.record_and_check_rate(ctx.session_id, Instant::now()) {
            return OverseerDecision::Block {
                reason: format!(
                    "session exceeded {} tool calls per minute",
                    self.config.deterministic.max_tool_calls_per_minute
                ),
            };
        }

        // 4. Default — allow.
        OverseerDecision::Allow
    }

    /// Evaluate a tool's output after it runs.
    ///
    /// Why: post-execution oversight (token-budget halting) is handled by the
    /// separate optimizer pipeline today; the deterministic overseer only
    /// monitors here.
    /// What: always returns `Allow`.
    /// Test: `post_tool_use_is_monitoring_only`.
    fn post_tool_use(&self, _ctx: &OverseerContext, _output: &str) -> OverseerDecision {
        OverseerDecision::Allow
    }

    /// Evaluate a question the session is asking the operator.
    ///
    /// Why: routine confirmation prompts ("shall I proceed?") can be answered
    /// automatically; anything else must reach a human.
    /// What: when disabled, escalates with `FlagForHuman`. Otherwise a
    /// case-insensitive substring match against the `auto_responses` map yields
    /// `Respond` with the canned reply; no match yields `FlagForHuman`.
    /// Test: `auto_responder_matches`, `auto_responder_is_case_insensitive`,
    /// `unknown_question_flags_for_human`.
    fn session_question(&self, _ctx: &OverseerContext, question: &str) -> OverseerDecision {
        if !self.config.enabled {
            return OverseerDecision::FlagForHuman {
                summary: format!("overseer disabled; question needs review: {question}"),
            };
        }
        for (pattern, response) in &self.config.auto_responses {
            if contains_ci(question, pattern) {
                return OverseerDecision::Respond {
                    text: response.clone(),
                };
            }
        }
        OverseerDecision::FlagForHuman {
            summary: format!("no auto-response for question: {question}"),
        }
    }

    /// Whether oversight is active.
    fn is_enabled(&self) -> bool {
        self.config.enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::overseer_config::DeterministicConfig;

    fn enabled_config() -> OverseerConfig {
        OverseerConfig {
            enabled: true,
            ..OverseerConfig::default()
        }
    }

    fn ctx_with_input(input: &str) -> OverseerContext {
        OverseerContext::new(
            SessionId::new(),
            "tmpm-test-session",
            Some("Bash".into()),
            Some(input.into()),
        )
    }

    #[test]
    fn blocks_blocklisted_input() {
        let mut cfg = enabled_config();
        cfg.deterministic.blocklist = vec!["rm -rf /".into()];
        let overseer = DeterministicOverseer::new(cfg);
        let decision = overseer.pre_tool_use(&ctx_with_input("sudo rm -rf / --no-preserve-root"));
        assert!(matches!(decision, OverseerDecision::Block { .. }));
    }

    #[test]
    fn allows_auto_approved_input() {
        let mut cfg = enabled_config();
        cfg.deterministic.auto_approve = vec!["git status".into()];
        let overseer = DeterministicOverseer::new(cfg);
        let decision = overseer.pre_tool_use(&ctx_with_input("git status --short"));
        assert_eq!(decision, OverseerDecision::Allow);
    }

    #[test]
    fn blocklist_takes_precedence_over_auto_approve() {
        // An input matching both lists is blocked — safety wins.
        let mut cfg = enabled_config();
        cfg.deterministic.blocklist = vec!["DROP TABLE".into()];
        cfg.deterministic.auto_approve = vec!["SELECT".into()];
        let overseer = DeterministicOverseer::new(cfg);
        let decision = overseer.pre_tool_use(&ctx_with_input("SELECT 1; DROP TABLE users"));
        assert!(matches!(decision, OverseerDecision::Block { .. }));
    }

    #[test]
    fn rate_limiter_blocks_after_limit() {
        // With a limit of N, the first N calls allow and the (N+1)th blocks.
        let cfg = OverseerConfig {
            enabled: true,
            deterministic: DeterministicConfig {
                max_tool_calls_per_minute: 3,
                ..DeterministicConfig::default()
            },
            ..OverseerConfig::default()
        };
        let overseer = DeterministicOverseer::new(cfg);
        let session = SessionId::new();
        let ctx = OverseerContext::new(session, "tmpm-rate-test", Some("Bash".into()), None);
        for _ in 0..3 {
            assert_eq!(overseer.pre_tool_use(&ctx), OverseerDecision::Allow);
        }
        let decision = overseer.pre_tool_use(&ctx);
        assert!(
            matches!(decision, OverseerDecision::Block { .. }),
            "N+1th call must be blocked"
        );
    }

    #[test]
    fn rate_limiter_is_per_session() {
        // One session hitting its limit must not block a different session.
        let cfg = OverseerConfig {
            enabled: true,
            deterministic: DeterministicConfig {
                max_tool_calls_per_minute: 1,
                ..DeterministicConfig::default()
            },
            ..OverseerConfig::default()
        };
        let overseer = DeterministicOverseer::new(cfg);
        let busy = OverseerContext::new(SessionId::new(), "tmpm-busy", Some("Bash".into()), None);
        let calm = OverseerContext::new(SessionId::new(), "tmpm-calm", Some("Bash".into()), None);
        assert_eq!(overseer.pre_tool_use(&busy), OverseerDecision::Allow);
        assert!(matches!(
            overseer.pre_tool_use(&busy),
            OverseerDecision::Block { .. }
        ));
        // The second session is unaffected.
        assert_eq!(overseer.pre_tool_use(&calm), OverseerDecision::Allow);
    }

    #[test]
    fn disabled_overseer_allows_everything() {
        // With enabled = false, even blocklisted input is allowed and the
        // rate limiter is never consulted.
        let mut cfg = OverseerConfig::default(); // disabled
        cfg.deterministic.blocklist = vec!["rm -rf /".into()];
        let overseer = DeterministicOverseer::new(cfg);
        assert_eq!(
            overseer.pre_tool_use(&ctx_with_input("rm -rf /")),
            OverseerDecision::Allow
        );
        assert!(!overseer.is_enabled());
    }

    #[test]
    fn post_tool_use_is_monitoring_only() {
        let overseer = DeterministicOverseer::new(enabled_config());
        let decision = overseer.post_tool_use(&ctx_with_input("anything"), "some output");
        assert_eq!(decision, OverseerDecision::Allow);
    }

    #[test]
    fn auto_responder_matches() {
        let mut cfg = enabled_config();
        cfg.auto_responses
            .insert("shall i proceed".into(), "yes, proceed".into());
        let overseer = DeterministicOverseer::new(cfg);
        let ctx = ctx_with_input("");
        let decision = overseer.session_question(&ctx, "Shall I proceed with the commit?");
        assert_eq!(
            decision,
            OverseerDecision::Respond {
                text: "yes, proceed".into()
            }
        );
    }

    #[test]
    fn auto_responder_is_case_insensitive() {
        let mut cfg = enabled_config();
        cfg.auto_responses
            .insert("READY TO CONTINUE".into(), "yes, continue".into());
        let overseer = DeterministicOverseer::new(cfg);
        let ctx = ctx_with_input("");
        let decision = overseer.session_question(&ctx, "are you ready to continue now?");
        assert_eq!(
            decision,
            OverseerDecision::Respond {
                text: "yes, continue".into()
            }
        );
    }

    #[test]
    fn unknown_question_flags_for_human() {
        let mut cfg = enabled_config();
        cfg.auto_responses
            .insert("shall i proceed".into(), "yes".into());
        let overseer = DeterministicOverseer::new(cfg);
        let ctx = ctx_with_input("");
        let decision = overseer.session_question(&ctx, "Should I delete the production database?");
        assert!(matches!(decision, OverseerDecision::FlagForHuman { .. }));
    }

    #[test]
    fn config_accessor_returns_policy() {
        let overseer = DeterministicOverseer::new(enabled_config());
        assert!(overseer.config().enabled);
    }
}
