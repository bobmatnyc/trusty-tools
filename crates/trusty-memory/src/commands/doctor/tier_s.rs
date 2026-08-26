//! Tier S re-affirmation check for `trusty-memory doctor` (#4890).
//!
//! Why: ADR-0028 D8 caps Tier S at 20 facts, which stops the always-injected
//! surface from *growing*. It does nothing about a rule that was true when
//! written and quietly stopped being true — that rule keeps its slot forever,
//! is re-transmitted on every turn of every agent session, and nothing in the
//! system can tell it has gone stale, because only its author can. D8 point 4's
//! answer is a cadence: surface anything unaffirmed for a quarter and let a
//! human decide. This module is that surface.
//!
//! What: fetches the live Tier S surface from the daemon's
//! [`PROMPT_FACTS_METHOD`], partitions it with
//! [`crate::prompt_facts::stale_tier_s_facts`], and renders a `CheckResult`.
//!
//! **It never retires anything.** Promotion and retirement of a standing rule
//! are deliberate human acts (D8 point 3); a diagnostic that silently evicted
//! them would break the exact guarantee the write-time cap exists to protect.
//! The strongest verdict this check can return is `Warn`, and even that is a
//! deliberate choice: a stale rule is *unreviewed*, not broken, so it must not
//! flip `doctor`'s exit code and turn a judgment call into a red build.
//!
//! #6286 moved the fetch off HTTP. It read `read_daemon_addr` and GET
//! `/api/v1/kg/prompt-facts`; ADR-0032 retired both, and — because nothing
//! referenced a deleted item — it would have gone on compiling while reporting
//! `Unknown` on every run, which is exactly the silent degradation this check
//! exists to avoid. It now calls the `list_prompt_facts` method over the
//! daemon's socket.
//!
//! Test: `no_facts_is_pass`, `fresh_facts_are_pass`,
//! `stale_facts_warn_and_name_the_retirement_path`, `stale_facts_never_fail`
//! (in the sibling `tier_s_tests.rs`) cover the interpreter fully;
//! `list_prompt_facts_endpoint_returns_hot_triples` pins the wire shape this
//! decodes, decoding it the same way.

use std::time::Duration;

use super::CheckResult;
use crate::prompt_facts::{
    render_stale_tier_s_report, stale_tier_s_facts, TierSFact, PROMPT_FACTS_METHOD,
    TIER_S_MAX_FACTS, TIER_S_REAFFIRM_DAYS,
};

/// Budget for the prompt-facts fetch.
///
/// Why: this is a read of at most [`TIER_S_MAX_FACTS`] rows out of an in-memory
/// registry walk — nothing like the process-introspection work `/health` does,
/// so it does not need `/health`'s 10 s allowance. 5 s is generous for the work
/// and short enough that a wedged daemon cannot stall the whole `doctor` run.
/// Test: not directly — exhausting it lands in the fetch's error branch, which
/// returns `Unknown`; that branch needs a real listener and is exercised by
/// running `trusty-memory doctor` against a live daemon.
const FETCH_TIMEOUT: Duration = Duration::from_secs(5);

/// Report Tier S facts overdue for re-affirmation.
///
/// Why: see the module docs — this is ADR-0028 D8 point 4.
/// What: calls [`PROMPT_FACTS_METHOD`] on the daemon's socket and hands the rows
/// to [`interpret_tier_s_facts`]. Every failure to reach or decode the daemon
/// yields `Unknown`, never `Pass`: not knowing whether standing rules are stale
/// is not the same as knowing they are fresh.
/// Test: `no_facts_is_pass`, `fresh_facts_are_pass`,
/// `stale_facts_warn_and_name_the_retirement_path`, `stale_facts_never_fail`
/// cover the interpreter; the transport is exercised end-to-end by running
/// `trusty-memory doctor` against a live daemon.
pub async fn check_tier_s_reaffirmation() -> CheckResult {
    let label = "Tier S re-affirmation".to_string();

    let answer = match crate::client::call_with_timeout(
        PROMPT_FACTS_METHOD,
        serde_json::json!({}),
        FETCH_TIMEOUT,
    )
    .await
    {
        Ok(answer) => answer,
        Err(e) => {
            return CheckResult::unknown(
                label,
                format!(
                    "{PROMPT_FACTS_METHOD} did not answer ({e:#}) — the Tier S surface lives in \
                     the daemon's palace registry, so its freshness is UNKNOWN while the daemon \
                     is unreachable. Start it with `trusty-memory service start` and re-run."
                ),
            );
        }
    };

    // `{"facts": [...]}`, not a bare array — see PROMPT_FACTS_METHOD.
    let facts: Vec<TierSFact> = match answer.get("facts").cloned() {
        Some(rows) => match serde_json::from_value(rows) {
            Ok(facts) => facts,
            Err(e) => {
                return CheckResult::unknown(
                    label,
                    format!(
                        "{PROMPT_FACTS_METHOD} answered, but the rows could not be decoded as \
                         Tier S facts: {e}. A daemon predating #4890 does not report \
                         `affirmed_at`; upgrade it to get a real answer."
                    ),
                );
            }
        },
        None => {
            return CheckResult::unknown(
                label,
                format!(
                    "{PROMPT_FACTS_METHOD} answered without a `facts` array: {answer}. Tier S \
                     freshness is UNKNOWN."
                ),
            );
        }
    };

    interpret_tier_s_facts(label, &facts, chrono::Utc::now())
}

/// Turn a Tier S snapshot into a verdict.
///
/// Why: split from the fetch so the judgment — which is the whole check — is
/// testable against a fixed clock and a fixed surface, with no daemon, no
/// network, and no dependence on whatever happens to be listening on 7070-7079
/// while the suite runs (#4897).
/// What: `Pass` when every fact was affirmed within [`TIER_S_REAFFIRM_DAYS`]
/// (including the empty surface, which is trivially fresh), `Warn` otherwise —
/// naming each stale rule, its age, and `remove_prompt_fact` as the retirement
/// path, exactly as the cap's refusal message names the current 20. Never
/// `Fail`: see the module docs.
/// Test: `no_facts_is_pass`, `fresh_facts_are_pass`,
/// `stale_facts_warn_and_name_the_retirement_path`,
/// `stale_facts_never_fail`.
pub(super) fn interpret_tier_s_facts(
    label: String,
    facts: &[TierSFact],
    now: chrono::DateTime<chrono::Utc>,
) -> CheckResult {
    let stale = stale_tier_s_facts(facts, now);
    if stale.is_empty() {
        return CheckResult::pass(
            label,
            format!(
                "{} of {TIER_S_MAX_FACTS} standing facts active, all affirmed within \
                 {TIER_S_REAFFIRM_DAYS} days",
                facts.len()
            ),
        );
    }
    CheckResult::warn(
        label,
        format!(
            "{} of {} active standing fact(s) have not been re-affirmed in {TIER_S_REAFFIRM_DAYS} \
             days (ADR-0028 D8). Tier S is injected into every turn of every session, so a rule \
             that stopped being true is paid for on every turn until someone notices. Nothing was \
             removed — retirement is a deliberate human act. Re-affirm a rule by asserting it \
             again with `kg_assert` (re-asserting it verbatim counts), or retire it with \
             `remove_prompt_fact` passing its `subject` and `predicate`. Overdue:{}",
            stale.len(),
            facts.len(),
            render_stale_tier_s_report(&stale),
        ),
    )
}

#[cfg(test)]
#[path = "tier_s_tests.rs"]
mod tests;
