//! Trigger classification — decide live vs dry-run for a review (REV-703).
//!
//! Why: a `review_requested` webhook must decide whether the resulting review
//! is *posted* (live) or merely *logged* (dry-run).  Per #582/REV-703 the bot
//! posts live only when it (or a configured allowlisted login) is the requested
//! reviewer; every other reviewer gets a dry-run.  Centralising this rule keeps
//! the webhook handler and the runner consistent and testable.
//!
//! What: `classify_review_request` maps a requested-reviewer login to a
//! `TriggerDecision` (`ForceLive`, `ForceDryRun`, or `None`), and
//! `effective_dry_run` folds that decision with the global `dry_run` flag using
//! the spec formula `(config.dry_run OR force_dry_run) AND NOT force_live`.
//!
//! Test: `bot_login_forces_live`, `allowlisted_login_forces_live`,
//! `other_reviewer_forces_dry_run`, `effective_dry_run_formula`.

use crate::config::ReviewConfig;

/// The dry-run override implied by who requested the review.
///
/// Why: the webhook needs a single value to thread to the runner so the runner
/// does not re-derive the policy; the three states cover the REV-703 table.
/// What: `ForceLive` (bot/allowlisted requester), `ForceDryRun` (any other
/// reviewer), or `None` (no override — fall back to the global flag).
/// Test: `bot_login_forces_live`, `other_reviewer_forces_dry_run`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TriggerDecision {
    /// Post the review live regardless of the global `dry_run` flag.
    ForceLive,
    /// Force a dry-run regardless of the global `dry_run` flag.
    ForceDryRun,
    /// No override — defer to the global `dry_run` flag.
    #[default]
    None,
}

impl TriggerDecision {
    /// Whether this decision forces a live (posted) review.
    pub fn is_force_live(self) -> bool {
        matches!(self, TriggerDecision::ForceLive)
    }

    /// Whether this decision forces a dry-run.
    pub fn is_force_dry_run(self) -> bool {
        matches!(self, TriggerDecision::ForceDryRun)
    }
}

/// Classify a `review_requested` event by its requested-reviewer login.
///
/// Why: this is the REV-703 gate that decides whether trusty-review posts or
/// only logs — the difference between an actionable review and a silent one.
/// What: returns `ForceLive` when `requested_login` equals the configured bot
/// username (case-insensitive) or appears in `live_review_requesters`;
/// returns `ForceDryRun` for any other non-empty reviewer; returns `None` when
/// no reviewer login is present (e.g. a team request) so the caller falls back
/// to the global flag.
/// Test: `bot_login_forces_live`, `allowlisted_login_forces_live`,
/// `other_reviewer_forces_dry_run`, `no_reviewer_is_none`.
pub fn classify_review_request(
    config: &ReviewConfig,
    requested_login: Option<&str>,
) -> TriggerDecision {
    let Some(login) = requested_login.map(str::trim).filter(|s| !s.is_empty()) else {
        return TriggerDecision::None;
    };

    if login.eq_ignore_ascii_case(&config.bot_username) {
        return TriggerDecision::ForceLive;
    }
    let login_lc = login.to_lowercase();
    if config.live_review_requesters.iter().any(|r| r == &login_lc) {
        return TriggerDecision::ForceLive;
    }
    TriggerDecision::ForceDryRun
}

/// Compute the effective dry-run flag from the global config and the trigger.
///
/// Why: the runner must obey the spec's exact precedence so a forced-live
/// review always posts even when the service default is dry-run, and a
/// forced-dry-run never posts even when the service default is live.
/// What: implements `effective_dry_run = (config.dry_run OR force_dry_run) AND
/// NOT force_live`.
/// Test: `effective_dry_run_formula`.
pub fn effective_dry_run(config_dry_run: bool, decision: TriggerDecision) -> bool {
    (config_dry_run || decision.is_force_dry_run()) && !decision.is_force_live()
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with(bot: &str, requesters: &[&str]) -> ReviewConfig {
        let mut c = ReviewConfig::load(None);
        c.bot_username = bot.to_string();
        c.live_review_requesters = requesters.iter().map(|s| s.to_lowercase()).collect();
        c
    }

    #[test]
    fn bot_login_forces_live() {
        let config = config_with("trusty-review[bot]", &[]);
        let d = classify_review_request(&config, Some("trusty-review[bot]"));
        assert_eq!(d, TriggerDecision::ForceLive);
    }

    #[test]
    fn bot_login_case_insensitive() {
        let config = config_with("Trusty-Review[bot]", &[]);
        let d = classify_review_request(&config, Some("trusty-review[bot]"));
        assert_eq!(d, TriggerDecision::ForceLive);
    }

    #[test]
    fn allowlisted_login_forces_live() {
        let config = config_with("trusty-review[bot]", &["alice", "bob"]);
        // Case-insensitive match against the (lowercased) allowlist.
        let d = classify_review_request(&config, Some("Alice"));
        assert_eq!(d, TriggerDecision::ForceLive);
    }

    #[test]
    fn other_reviewer_forces_dry_run() {
        let config = config_with("trusty-review[bot]", &["alice"]);
        let d = classify_review_request(&config, Some("random-human"));
        assert_eq!(d, TriggerDecision::ForceDryRun);
    }

    #[test]
    fn no_reviewer_is_none() {
        let config = config_with("trusty-review[bot]", &[]);
        assert_eq!(
            classify_review_request(&config, None),
            TriggerDecision::None
        );
        assert_eq!(
            classify_review_request(&config, Some("   ")),
            TriggerDecision::None
        );
    }

    #[test]
    fn effective_dry_run_formula() {
        // force_live always posts, even when config default is dry-run.
        assert!(!effective_dry_run(true, TriggerDecision::ForceLive));
        // force_dry_run never posts, even when config default is live.
        assert!(effective_dry_run(false, TriggerDecision::ForceDryRun));
        // No override → defer to config default.
        assert!(effective_dry_run(true, TriggerDecision::None));
        assert!(!effective_dry_run(false, TriggerDecision::None));
    }
}
