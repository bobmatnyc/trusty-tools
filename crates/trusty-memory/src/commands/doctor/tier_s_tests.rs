//! Unit tests for the Tier S re-affirmation doctor check (#4890).
//!
//! These live in a sibling file because `doctor/mod.rs` is already at 472 SLOC
//! against a 500 cap, so the suite could not go there either; a sibling
//! `*_tests.rs` is this repo's established answer (see `memory_core/filter.rs`).
//!
//! Why: the check's judgment must be provable without a daemon. Two
//! `check_daemon_health_*` tests in this same module already fail whenever a
//! live trusty-memory daemon happens to be listening on 7070-7079 (#4897) —
//! that is exactly the coupling these tests must not repeat, so every case here
//! drives [`super::interpret_tier_s_facts`] with a fixed clock and a literal
//! fact list.
//! What: covers the empty surface, the all-fresh surface, the stale surface's
//! wording, and the invariant that a stale surface never escalates past `Warn`.
//! Test: this file is the suite.

use super::super::CheckStatus;
use super::interpret_tier_s_facts;
use crate::prompt_facts::{TierSFact, TIER_S_REAFFIRM_DAYS};
use chrono::{DateTime, Duration, Utc};

/// A fixed clock so no test depends on wall time.
fn now() -> DateTime<Utc> {
    DateTime::from_timestamp(1_800_000_000, 0).expect("fixed now")
}

fn fact(subject: &str, object: &str, days_ago: i64) -> TierSFact {
    TierSFact {
        subject: subject.into(),
        predicate: "has_convention".into(),
        object: object.into(),
        affirmed_at: now() - Duration::days(days_ago),
    }
}

/// Why: an empty Tier S surface is the state ADR-0028 §C3 says the estate
/// starts from. Reporting "nothing to re-affirm" as anything but a pass would
/// make `doctor` yellow on a correct, brand-new install.
/// What: asserts `Pass` for an empty fact list.
/// Test: itself.
#[test]
fn no_facts_is_pass() {
    let r = interpret_tier_s_facts("Tier S".into(), &[], now());
    assert_eq!(r.status, CheckStatus::Pass);
}

/// Why: the check must not nag about rules that were reviewed this quarter, or
/// operators will learn to ignore it — which is the failure mode that made
/// every soft mechanism before the cap useless (D8).
/// What: two facts inside the window, including one sitting exactly on the
/// threshold day, assert `Pass`.
/// Test: itself.
#[test]
fn fresh_facts_are_pass() {
    let facts = vec![
        fact("conv-1", "Write plainly", 3),
        fact("conv-2", "Never merge red CI", TIER_S_REAFFIRM_DAYS),
    ];
    let r = interpret_tier_s_facts("Tier S".into(), &facts, now());
    assert_eq!(r.status, CheckStatus::Pass, "detail: {:?}", r.detail);
}

/// Why: the deliverable of a report-only check is the report. An operator who
/// reads it must be able to act without a second lookup, so the message has to
/// carry the rule text, its age, and the `remove_prompt_fact` retirement path
/// — the same standard the cap's refusal message meets. It must also say
/// plainly that nothing was removed, because "doctor flagged my standing rules"
/// otherwise reads as "doctor deleted my standing rules".
/// What: one stale and one fresh fact; asserts `Warn`, asserts the stale rule
/// and its age appear, asserts the fresh one does not, and asserts both the
/// re-affirmation and retirement paths are named.
/// Test: itself.
#[test]
fn stale_facts_warn_and_name_the_retirement_path() {
    let facts = vec![
        fact("conv-old", "Clickable links always", 200),
        fact("conv-new", "Write plainly", 1),
    ];
    let r = interpret_tier_s_facts("Tier S".into(), &facts, now());
    assert_eq!(r.status, CheckStatus::Warn);
    let d = r.detail.as_deref().unwrap_or("");
    assert!(d.contains("conv-old"), "must name the stale subject: {d}");
    assert!(
        d.contains("Clickable links always"),
        "must show the rule text: {d}"
    );
    assert!(d.contains("200d ago"), "must show the age: {d}");
    assert!(
        !d.contains("conv-new"),
        "must not list a fact affirmed 1 day ago: {d}"
    );
    assert!(
        d.contains("remove_prompt_fact"),
        "must name the retirement path: {d}"
    );
    assert!(
        d.contains("kg_assert"),
        "must name the re-affirmation path: {d}"
    );
    assert!(
        d.contains("Nothing was removed"),
        "must state that it did not retire anything: {d}"
    );
    assert!(
        d.contains("1 of 2"),
        "must report stale-of-total so the scale is legible: {d}"
    );
}

/// Why: this is the ticket's hard constraint, not a nicety. Promotion and
/// retirement of a standing rule are deliberate human acts (ADR-0028 D8 point
/// 3). `Fail` flips `doctor`'s exit code, so a `Fail` here would let a stale
/// rule break a scripted health gate and pressure someone into deleting it
/// without review — a slower path to the same silent eviction the check exists
/// to avoid.
/// What: a surface that is entirely, extremely stale still yields `Warn`.
/// Test: itself.
#[test]
fn stale_facts_never_fail() {
    let facts: Vec<TierSFact> = (0..20)
        .map(|i| fact(&format!("conv-{i}"), "an ancient rule", 5_000))
        .collect();
    let r = interpret_tier_s_facts("Tier S".into(), &facts, now());
    assert_eq!(
        r.status,
        CheckStatus::Warn,
        "a report-only check must never escalate to Fail"
    );
    assert_ne!(r.status, CheckStatus::Fail);
}
