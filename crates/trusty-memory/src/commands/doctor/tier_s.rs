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
//! `GET /api/v1/kg/prompt-facts`, partitions it with
//! [`crate::prompt_facts::stale_tier_s_facts`], and renders a `CheckResult`.
//!
//! **It never retires anything.** Promotion and retirement of a standing rule
//! are deliberate human acts (D8 point 3); a diagnostic that silently evicted
//! them would break the exact guarantee the write-time cap exists to protect.
//! The strongest verdict this check can return is `Warn`, and even that is a
//! deliberate choice: a stale rule is *unreviewed*, not broken, so it must not
//! flip `doctor`'s exit code and turn a judgment call into a red build.
//!
//! Test: `no_facts_is_pass`, `fresh_facts_are_pass`,
//! `stale_facts_warn_and_name_the_retirement_path`, `stale_facts_never_fail`
//! (in the sibling `tier_s_tests.rs`) cover the interpreter fully; the thin
//! HTTP fetch below is not unit-tested for the same reason
//! `check_daemon_health`'s transport is not — it needs a live listener.

use std::time::Duration;

use super::CheckResult;
use crate::prompt_facts::{
    render_stale_tier_s_report, stale_tier_s_facts, TierSFact, TIER_S_MAX_FACTS,
    TIER_S_REAFFIRM_DAYS,
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

/// HTTP path serving the Tier S surface with `affirmed_at` per row.
pub(super) const PROMPT_FACTS_PATH: &str = "/api/v1/kg/prompt-facts";

/// Report Tier S facts overdue for re-affirmation.
///
/// Why: see the module docs — this is ADR-0028 D8 point 4.
/// What: resolves the daemon address, fetches [`PROMPT_FACTS_PATH`], and hands
/// the rows to [`interpret_tier_s_facts`]. Every failure to reach or decode the
/// daemon yields `Unknown`, never `Pass`: not knowing whether standing rules
/// are stale is not the same as knowing they are fresh.
/// Test: `no_facts_is_pass`, `fresh_facts_are_pass`,
/// `stale_facts_warn_and_name_the_retirement_path`, `stale_facts_never_fail`
/// cover the interpreter; the transport is exercised end-to-end by running
/// `trusty-memory doctor` against a live daemon.
pub async fn check_tier_s_reaffirmation() -> CheckResult {
    let label = "Tier S re-affirmation".to_string();

    let addr = match trusty_common::read_daemon_addr("trusty-memory") {
        Ok(Some(a)) => a,
        Ok(None) => {
            return CheckResult::unknown(
                label,
                "no daemon address recorded — the Tier S surface lives in the daemon's palace \
                 registry, so its freshness is UNKNOWN while the daemon is down. Start it with \
                 `trusty-memory service start` and re-run."
                    .to_string(),
            );
        }
        Err(e) => {
            return CheckResult::unknown(
                label,
                format!("could not read the daemon address file: {e:#} — freshness is UNKNOWN"),
            );
        }
    };
    let base = if addr.starts_with("http://") || addr.starts_with("https://") {
        addr
    } else {
        format!("http://{addr}")
    };
    let url = format!("{base}{PROMPT_FACTS_PATH}");

    let client = match reqwest::Client::builder().timeout(FETCH_TIMEOUT).build() {
        Ok(c) => c,
        Err(e) => {
            return CheckResult::unknown(label, format!("could not build HTTP client: {e}"));
        }
    };

    let facts: Vec<TierSFact> = match client.get(&url).send().await {
        Ok(resp) if resp.status().is_success() => match resp.json().await {
            Ok(f) => f,
            Err(e) => {
                return CheckResult::unknown(
                    label,
                    format!(
                        "{url} answered, but the body could not be decoded as Tier S facts: {e}. \
                         A daemon predating #4890 does not report `affirmed_at`; upgrade it to \
                         get a real answer."
                    ),
                );
            }
        },
        Ok(resp) => {
            return CheckResult::unknown(label, format!("{url} → {}", resp.status()));
        }
        Err(e) => {
            return CheckResult::unknown(
                label,
                format!("{url} did not answer ({e}) — Tier S freshness is UNKNOWN"),
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

/// Unit tests live in a sibling file so this module stays under the 500-SLOC
/// production cap (the test file is classified as a test target).
///
/// Why: `doctor/mod.rs` is already at 472 SLOC against a 500 cap, so the suite
/// could not go there either; a sibling `*_tests.rs` is this repo's established
/// answer (see `memory_core/filter.rs`).
/// What: pulls in `tier_s_tests.rs` as the `tests` module under `cfg(test)`.
/// Test: the referenced file is itself the test suite.
#[cfg(test)]
#[path = "tier_s_tests.rs"]
mod tests;
