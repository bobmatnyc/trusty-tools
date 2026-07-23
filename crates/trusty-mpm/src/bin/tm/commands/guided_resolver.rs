//! The nested-session guard's name/id resolver + duplicate-name tie-break
//! (#2157 item 4, #3714).
//!
//! Why: extracted from `guided.rs` (issue #3714 line-cap follow-up — the
//! finding-1/finding-2 review round pushed `guided.rs` to 533 SLOC, over the
//! 500-SLOC production cap) so the resolver logic and its extensive tie-break
//! documentation live in their own file, mirroring the existing
//! `guided_autostart`/`guided_inplace`/`guided_launch`/`guided_resume`
//! sibling-module convention this crate already uses for `guided.rs`'s other
//! extracted concerns. Pure code motion — no behavior change; see each
//! function's doc for the full rationale (unchanged from the pre-move
//! version).
//! What: [`nested_managed_match`] — does any known managed record belong to
//! the pane bare `tm` is currently running inside, matched by tmux session
//! name or `TM_MANAGED_SESSION_ID`? [`pick_least_ambiguous`] — the tie-break
//! tiers applied when more than one record matches (a duplicated tmux
//! session name, #3692/#3714). [`print_ambiguous_match_notice`] — the
//! full-session-id disambiguation listing printed when every tier is
//! exhausted and a tie remains.
//! Test: `guided_resolver_tests.rs` (moved here from `tests_behavior_c_tests.rs`
//! in the same #3714 line-cap follow-up, for the identical reason).

/// Pure predicate: does ANY record in `records` belong to the pane bare `tm`
/// is currently running inside?
///
/// Why: isolating the match rule from the daemon round-trip and the tmux
/// shell-out makes the actual guard DECISION exhaustively unit-testable
/// without a live daemon or tmux server.
///
/// #2790 code-critic HIGH: tmux SESSION NAMES ARE RECYCLED after a session is
/// decommissioned (`session_manager::manager`'s name-reuse policy) — a plain
/// `.find()` over ALL candidates (including tombstones) can therefore return a
/// STALE `Decommissioned` record for a name that a genuinely LIVE session now
/// also owns, which would drive `RelaunchInPlace` against the wrong (dead)
/// record while a live session sharing that name sits in a sibling pane — the
/// exact hijack class #2456/#2680 fixed elsewhere.
/// [`SessionManager::capture_pane_by_tmux_name`] (`session_manager/manager.rs`)
/// already carries the identical guard for the same reason (idle-reaper pane
/// capture) — this mirrors it.
/// What: collects every record matching `current_session_name` OR
/// `env_session_id`, then prefers the most-recently-created (`created_at`,
/// RFC3339 — lexicographically sortable) NON-`Decommissioned` match. Only when
/// NO candidate is non-`Decommissioned` does it fall back to the
/// most-recently-created `Decommissioned` candidate — the legitimate #2777
/// repro case (a decommissioned session's OWN, not-yet-recycled name). `None`
/// when both inputs are `None`/non-matching, which includes the "not inside
/// tmux" case (callers pass `current_session_name: None` then).
/// Test: `nested_managed_match_by_session_name`,
/// `nested_managed_match_by_env_id`, `nested_managed_match_none_when_no_match`,
/// `nested_managed_match_none_when_both_inputs_absent`,
/// `nested_managed_match_prefers_live_over_recycled_decommissioned_name`,
/// `nested_managed_match_falls_back_to_decommissioned_when_no_live_candidate`,
/// `nested_managed_match_prefers_most_recent_live_among_multiple`,
/// `nested_managed_match_prefers_own_pane_identity_over_recency` (#3714),
/// `nested_managed_match_prefers_verifiably_alive_pane_over_dead_duplicate`
/// (#3714), `nested_managed_match_refuses_and_lists_ids_when_still_tied`
/// (#3714) — see [`pick_least_ambiguous`] for the tie-break tiers.
pub(crate) fn nested_managed_match<'a>(
    current_session_name: Option<&str>,
    env_session_id: Option<&str>,
    current_pane_id: Option<&str>,
    records: &'a [trusty_mpm::client::ManagedSessionSummary],
) -> Option<&'a trusty_mpm::client::ManagedSessionSummary> {
    let is_candidate = move |r: &&trusty_mpm::client::ManagedSessionSummary| {
        current_session_name.is_some_and(|n| n == r.name)
            || env_session_id.is_some_and(|id| id == r.id)
    };
    let candidates: Vec<&trusty_mpm::client::ManagedSessionSummary> =
        records.iter().filter(is_candidate).collect();
    if candidates.is_empty() {
        return None;
    }
    match pick_least_ambiguous(candidates, current_pane_id) {
        Ok(picked) => Some(picked),
        Err(tied) => {
            // #3714: every tie-break tier is exhausted and more than one
            // candidate remains — refuse rather than silently pick a row
            // (the exact bug this issue reports: a name-keyed lookup landed
            // on a stale duplicate instead of the live, owner-occupied
            // session). Print the FULL session ids so the operator can
            // disambiguate manually; the caller falls through to the
            // ordinary picker/derive_project path on `None`.
            print_ambiguous_match_notice(
                current_session_name
                    .or(env_session_id)
                    .unwrap_or("<unknown>"),
                &tied,
            );
            None
        }
    }
}

/// Break a tie among several candidates [`nested_managed_match`] found by
/// name/env-id (#3714) — a duplicated tmux session name can legitimately
/// produce more than one match here (the exact #3692/#3714 condition).
///
/// Why: the pre-#3714 tie-break was a single `max_by_key(created_at)` over
/// every non-decommissioned candidate — deterministic, but blind to which
/// candidate's pane is ACTUALLY alive. A stale duplicate created AFTER the
/// genuinely live session (an entirely plausible ordering — the live session
/// long predates the stale one in the reported issue) would win purely on
/// recency, stranding the operator against a dead-pane refusal while sitting
/// inside their own live session.
/// What: applies FOUR tiers, strongest signal first, returning as soon as
/// one produces a single winner: (1) the invoking process's OWN tmux pane —
/// `pane_id` is never shared between two records, so a match here is proof,
/// not a heuristic; (2) among the remainder, prefer candidates whose `state`
/// reads `"active"` (server-reconciled PANE-scoped liveness, #3714 part 3 —
/// no longer a bare name-string check) over ones that don't; (3) — ONLY when
/// tier (2) is empty — prefer any NON-`"decommissioned"` candidate (e.g.
/// `"stopped"`/`"errored"`) over a decommissioned tombstone, EXACTLY
/// mirroring the pre-#3714 `state != "decommissioned"` tie-break (#3714
/// review finding 1: an earlier revision collapsed tiers 2 and 3 into a
/// single `state == "active"` filter, which let a genuinely `"stopped"`
/// record LOSE to a newer decommissioned tombstone — a real behavior
/// regression from the pre-#3714 code, not just a doc inaccuracy); falling
/// back to EVERY candidate only when tier (3) is also empty (every
/// candidate is decommissioned — the legitimate #2777 case); (4) within the
/// surviving tier, the most-recently-created candidate (the prior sole
/// tie-break, now the weakest and last resort). Returns `Ok(single winner)`
/// as soon as exactly one candidate survives a tier; `Err(tied)` — the
/// tier's full surviving set — when candidates remain tied even after tier
/// (4), so the caller can print every tied id rather than guess.
/// Test: see [`nested_managed_match`]'s test list.
pub(crate) fn pick_least_ambiguous<'a>(
    candidates: Vec<&'a trusty_mpm::client::ManagedSessionSummary>,
    current_pane_id: Option<&str>,
) -> Result<
    &'a trusty_mpm::client::ManagedSessionSummary,
    Vec<&'a trusty_mpm::client::ManagedSessionSummary>,
> {
    if candidates.len() == 1 {
        return Ok(candidates[0]);
    }
    // (1) proof-positive: this process's own tmux pane.
    if let Some(pane_id) = current_pane_id
        && let Some(&own) = candidates
            .iter()
            .find(|r| r.pane_id.as_deref() == Some(pane_id))
    {
        return Ok(own);
    }
    // (2) prefer verifiably-alive (server-reconciled `state == "active"`)
    // candidates; (3) failing that, ANY non-decommissioned candidate — this
    // second sub-tier is what the pre-#3714 code did unconditionally, and it
    // must still win over a decommissioned tombstone regardless of recency
    // (review finding 1); (4-fallback) only decommissioned candidates
    // remain (#2777).
    let alive: Vec<&trusty_mpm::client::ManagedSessionSummary> = candidates
        .iter()
        .copied()
        .filter(|r| r.state == "active")
        .collect();
    let tier = if !alive.is_empty() {
        alive
    } else {
        let non_decommissioned: Vec<&trusty_mpm::client::ManagedSessionSummary> = candidates
            .iter()
            .copied()
            .filter(|r| r.state != "decommissioned")
            .collect();
        if non_decommissioned.is_empty() {
            candidates
        } else {
            non_decommissioned
        }
    };
    // (4) most-recently-created within the surviving tier.
    let newest_created = tier
        .iter()
        .map(|r| r.created_at.clone().unwrap_or_default())
        .max()
        .unwrap_or_default();
    let newest: Vec<&trusty_mpm::client::ManagedSessionSummary> = tier
        .into_iter()
        .filter(|r| r.created_at.clone().unwrap_or_default() == newest_created)
        .collect();
    match newest.as_slice() {
        [only] => Ok(only),
        _ => Err(newest),
    }
}

/// Print the operator-facing disambiguation notice for
/// [`nested_managed_match`]'s refusal path (#3714) — every tied candidate's
/// FULL session id, never a truncated/ambiguous name.
///
/// Why: the whole point of refusing here is that a name/short identifier is
/// not enough to pick safely; the notice must give the operator the one
/// thing that IS unambiguous — the full id — so they can act with an
/// explicit `tm sessions resume <id>`.
/// What: writes one header line naming the ambiguous key, then one line per
/// tied candidate (`id`, `name`, `state`) to stderr.
/// Test: `nested_managed_match_refuses_and_lists_ids_when_still_tied`
/// asserts the returned `None`; this pure formatter's exact text is not
/// itself unit-asserted (stderr side effect), matching `guided.rs`'s own
/// convention for `print_project_context`/`print_non_tty_hint`.
fn print_ambiguous_match_notice(key: &str, tied: &[&trusty_mpm::client::ManagedSessionSummary]) {
    eprintln!(
        "tm: '{key}' matches {} managed sessions and none can be told apart automatically \
         — refusing to guess. Use an explicit id:",
        tied.len()
    );
    for r in tied {
        eprintln!("tm:   {} — {} (state={})", r.id, r.name, r.state);
    }
}

#[cfg(test)]
#[path = "guided_resolver_tests.rs"]
mod tests;
