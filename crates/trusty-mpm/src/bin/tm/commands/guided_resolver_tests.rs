//! Unit tests for [`super::nested_managed_match`] / [`super::pick_least_ambiguous`]
//! (#2157 item 4, #3714).
//!
//! Why: moved here from `tests_behavior_c_tests.rs` (issue #3714 line-cap
//! follow-up — the finding-1/finding-2 review round pushed that file to 1541
//! SLOC, over the 1500-SLOC test cap) alongside the production code's own
//! move into `guided_resolver.rs`, following this crate's existing
//! colocated-test-file convention (e.g. `session_manager/rename.rs` +
//! `rename_tests.rs`). Pure code motion — no behavior/assertion change from
//! the pre-move versions; `make_session`/`make_session_at` are local copies
//! of the identically-named fixtures still used elsewhere in
//! `tests_behavior_c_tests.rs` (which keeps its own copies — the two files
//! are independent, matching how every other colocated test file in this
//! crate builds its own minimal fixtures rather than sharing one).
//! What: the nested-session guard's pure match decision (by tmux session
//! name / `TM_MANAGED_SESSION_ID`) and its duplicate-name tie-break tiers
//! (own-pane identity → verifiably-alive → non-decommissioned →
//! most-recent → refuse-and-list).
//! Test: this file IS the test module; run with `cargo test -p trusty-mpm`.

use super::{nested_managed_match, pick_least_ambiguous};

/// Construct a minimal `ManagedSessionSummary` for tests.
fn make_session(
    name: &str,
    state: &str,
    last_activity_at: Option<&str>,
) -> trusty_mpm::client::ManagedSessionSummary {
    trusty_mpm::client::ManagedSessionSummary {
        id: format!("{name}-id"),
        name: name.to_string(),
        state: state.to_string(),
        persisted_state: None,
        workspace_path: None,
        repo_url: None,
        branch: None,
        created_at: None,
        last_activity_at: last_activity_at.map(str::to_owned),
        pending_decision: None,
        proposed_default: None,
        source_id: None,
        task: None,
        cwd: None,
        claude_session_id: None,
        deliverable_id: None,
        pane_id: None,
        injection_status: None,
        unresumable: false,
        stale_assets: false,
        attached: false,
        slot: 0,
        deleted: false,
    }
}

/// Build a [`make_session`] summary with an explicit `created_at` (RFC3339),
/// for tests that need to control recency ordering.
fn make_session_at(
    name: &str,
    state: &str,
    created_at: &str,
) -> trusty_mpm::client::ManagedSessionSummary {
    let mut s = make_session(name, state, None);
    s.created_at = Some(created_at.to_string());
    s
}

// ── nested_managed_match (#2157 item 4) ──────────────────────────────────────
// The nested-session guard's pure decision: does any known managed record
// belong to the pane bare `tm` is currently running inside? Matched either by
// tmux session name (the primary signal — works even when the env var was
// never exported into THIS particular pane) or by TM_MANAGED_SESSION_ID
// (belt-and-suspenders).

#[test]
fn nested_managed_match_by_session_name() {
    let sessions = vec![make_session("tm-proj-01", "active", None)];
    let matched = nested_managed_match(Some("tm-proj-01"), None, None, &sessions);
    assert_eq!(matched.map(|s| s.name.as_str()), Some("tm-proj-01"));
}

#[test]
fn nested_managed_match_by_env_id() {
    let sessions = vec![make_session("tm-proj-01", "active", None)];
    // make_session sets id = "<name>-id".
    let matched = nested_managed_match(None, Some("tm-proj-01-id"), None, &sessions);
    assert_eq!(matched.map(|s| s.name.as_str()), Some("tm-proj-01"));
}

#[test]
fn nested_managed_match_none_when_no_match() {
    let sessions = vec![make_session("tm-proj-01", "active", None)];
    // Neither the session name nor the env id matches any record — e.g. a
    // plain terminal opened outside any managed tmux session.
    let matched = nested_managed_match(
        Some("some-other-session"),
        Some("unrelated-id"),
        None,
        &sessions,
    );
    assert!(matched.is_none());
}

#[test]
fn nested_managed_match_none_when_both_inputs_absent() {
    // The "not inside tmux" case: the guard's I/O wrapper passes None for
    // both keys, which must never spuriously match any record.
    let sessions = vec![make_session("tm-proj-01", "active", None)];
    let matched = nested_managed_match(None, None, None, &sessions);
    assert!(matched.is_none());
}

#[test]
fn nested_managed_match_finds_record_missing_from_source_id_filtered_list() {
    // #2157 items 4+5 interplay: the guard fetches the UNFILTERED session
    // list specifically so it can still find a record whose source_id write
    // never landed (item 5's failure mode) — this record would be invisible
    // to a `?source_id=` filtered fetch, but the guard must still catch it by
    // tmux session name.
    let mut orphaned = make_session("tm-orphan-02", "active", None);
    orphaned.source_id = None;
    let sessions = vec![orphaned];
    let matched = nested_managed_match(Some("tm-orphan-02"), None, None, &sessions);
    assert!(
        matched.is_some(),
        "must match by session name regardless of source_id"
    );
}

#[test]
fn nested_managed_match_prefers_live_over_recycled_decommissioned_name() {
    // #2790 code-critic HIGH: tmux session names are RECYCLED after
    // decommission. A stale Decommissioned tombstone sharing a name with a
    // genuinely LIVE session must never win the match — liveness takes STRICT
    // precedence over recency (not just "usually more recent"). Proven here by
    // giving the tombstone a LATER created_at than the live record — an
    // adversarial ordering the pre-#2790 plain `.find()` would have been
    // vulnerable to depending on iteration/list order.
    let mut decommissioned =
        make_session_at("tm-proj-01", "decommissioned", "2026-03-01T00:00:00Z");
    decommissioned.id = "old-id".to_string();
    let mut live = make_session_at("tm-proj-01", "active", "2026-01-01T00:00:00Z");
    live.id = "new-id".to_string();
    let sessions = vec![decommissioned, live];

    let matched = nested_managed_match(Some("tm-proj-01"), None, None, &sessions);
    assert_eq!(
        matched.map(|s| s.id.as_str()),
        Some("new-id"),
        "a live record must win over a decommissioned one sharing the same \
         recycled name, even when the tombstone's created_at is later"
    );
}

#[test]
fn nested_managed_match_falls_back_to_decommissioned_when_no_live_candidate() {
    // The legitimate #2777 repro: the ONLY record sharing this tmux session
    // name IS the decommissioned session itself (its name has not yet been
    // recycled by a new session) — the guard must still match it so the
    // in-place revive path can run.
    let sessions = vec![make_session("tm-apex-01", "decommissioned", None)];
    let matched = nested_managed_match(Some("tm-apex-01"), None, None, &sessions);
    assert_eq!(
        matched.map(|s| s.name.as_str()),
        Some("tm-apex-01"),
        "must fall back to the decommissioned record when no live candidate \
         shares its name"
    );
}

#[test]
fn nested_managed_match_prefers_most_recent_live_among_multiple() {
    // Belt-and-suspenders: when MULTIPLE non-decommissioned candidates somehow
    // share a name (should not normally happen, but the guard must still be
    // deterministic), the most recently created one wins — mirroring
    // `capture_pane_by_tmux_name`'s `max_by_key(created_at)` convention.
    let mut older = make_session_at("tm-proj-02", "active", "2026-01-01T00:00:00Z");
    older.id = "older-id".to_string();
    let mut newer = make_session_at("tm-proj-02", "active", "2026-02-01T00:00:00Z");
    newer.id = "newer-id".to_string();
    let sessions = vec![older, newer];

    let matched = nested_managed_match(Some("tm-proj-02"), None, None, &sessions);
    assert_eq!(matched.map(|s| s.id.as_str()), Some("newer-id"));
}

// ── #3714: live-pane tie-break, pane-identity, and disambiguation refusal ────

#[test]
fn nested_managed_match_prefers_verifiably_alive_pane_over_dead_duplicate() {
    // The exact #3714 reproduction: a STALE duplicate sharing the live
    // session's name was created LATER than the live session — the pre-#3714
    // `max_by_key(created_at)` picked the stale one purely on recency. Since
    // #3714 part 3, `state` is server-reconciled PANE-scoped (not a bare
    // name-string check), so the stale duplicate correctly reads "stopped"
    // here even though its NAME is shared with a live session — and the
    // live-state tier must win regardless of which one is newer.
    let mut live = make_session_at("tm-tagents", "active", "2026-07-14T05:19:51Z");
    live.id = "f443c12d".to_string();
    let mut stale = make_session_at("tm-tagents", "stopped", "2026-07-18T19:17:52Z");
    stale.id = "7dabd521".to_string();
    let sessions = vec![stale, live];

    let matched = nested_managed_match(Some("tm-tagents"), None, None, &sessions);
    assert_eq!(
        matched.map(|s| s.id.as_str()),
        Some("f443c12d"),
        "the verifiably-alive record must win over a newer-but-dead duplicate"
    );
}

#[test]
fn nested_managed_match_prefers_non_decommissioned_over_newer_decommissioned_tombstone() {
    // #3714 review finding 1 (HIGH): when NEITHER candidate reads "active"
    // (so tier 2 is empty), a genuinely non-terminal record (e.g.
    // "stopped") must still beat a DECOMMISSIONED tombstone sharing its
    // name, regardless of which one is newer — this is the pre-#3714
    // `state != "decommissioned"` tie-break, which an earlier revision of
    // this fix accidentally dropped by collapsing tiers 2+3 into a single
    // `state == "active"` filter. Adversarial ordering: the tombstone is
    // FIVE MONTHS newer than the stopped record, proving this is a real
    // tier, not an artifact of recency.
    let mut stopped = make_session_at("tm-proj-03", "stopped", "2026-01-01T00:00:00Z");
    stopped.id = "stopped-id".to_string();
    let mut decommissioned =
        make_session_at("tm-proj-03", "decommissioned", "2026-06-01T00:00:00Z");
    decommissioned.id = "decommissioned-id".to_string();
    let sessions = vec![decommissioned, stopped];

    let matched = nested_managed_match(Some("tm-proj-03"), None, None, &sessions);
    assert_eq!(
        matched.map(|s| s.id.as_str()),
        Some("stopped-id"),
        "a non-decommissioned record must beat a newer decommissioned tombstone \
         when neither candidate is verifiably active"
    );
}

#[test]
fn nested_managed_match_prefers_own_pane_identity_over_recency() {
    // Bullet 2 of the proposed fix: when invoked from inside an existing
    // managed session, the invoking process's OWN tmux pane identity is
    // proof-positive and must win even over a newer AND "active"-reading
    // duplicate (an adversarial ordering that would otherwise defeat tier 2).
    let mut mine = make_session_at("tm-tagents", "active", "2026-01-01T00:00:00Z");
    mine.id = "mine-id".to_string();
    mine.pane_id = Some("%652".to_string());
    let mut other = make_session_at("tm-tagents", "active", "2026-06-01T00:00:00Z");
    other.id = "other-id".to_string();
    other.pane_id = Some("%999".to_string());
    let sessions = vec![other, mine];

    let matched = nested_managed_match(Some("tm-tagents"), None, Some("%652"), &sessions);
    assert_eq!(
        matched.map(|s| s.id.as_str()),
        Some("mine-id"),
        "the invoking process's own confirmed pane must win over a newer sibling"
    );
}

#[test]
fn nested_managed_match_refuses_and_lists_ids_when_still_tied() {
    // Every tie-break tier exhausted: two "active" candidates sharing an
    // identical created_at, no pane-identity signal available — must refuse
    // (`None`) rather than guess, never silently pick a row.
    let mut a = make_session_at("tm-dup-01", "active", "2026-01-01T00:00:00Z");
    a.id = "id-a".to_string();
    let mut b = make_session_at("tm-dup-01", "active", "2026-01-01T00:00:00Z");
    b.id = "id-b".to_string();
    let sessions = vec![a, b];

    let matched = nested_managed_match(Some("tm-dup-01"), None, None, &sessions);
    assert!(
        matched.is_none(),
        "an unresolvable tie must refuse, never silently pick either candidate"
    );
}

#[test]
fn pick_least_ambiguous_returns_tied_set_on_refusal() {
    // Direct coverage of the tie-break helper's `Err` branch — the caller
    // (`nested_managed_match`) uses this to print the full-id disambiguation
    // listing (#3714's "never silently pick a row" requirement).
    let mut a = make_session_at("tm-dup-02", "active", "2026-02-02T00:00:00Z");
    a.id = "id-a".to_string();
    let mut b = make_session_at("tm-dup-02", "active", "2026-02-02T00:00:00Z");
    b.id = "id-b".to_string();
    let candidates = vec![&a, &b];

    let result = pick_least_ambiguous(candidates, None);
    let tied = result.expect_err("a genuine tie must be Err, not a guessed Ok");
    assert_eq!(tied.len(), 2, "both tied candidates must be reported");
    let mut ids: Vec<&str> = tied.iter().map(|s| s.id.as_str()).collect();
    ids.sort_unstable();
    assert_eq!(ids, ["id-a", "id-b"]);
}
