//! Unit tests for the picker's `d <glob>` bulk delete (#5539).
//!
//! Why: the feature is destructive and bulk, so the tests that matter are the
//! ones proving it does NOT delete. `decide_glob_delete` is built so that
//! `GlobDeleteOutcome::Confirm` is the single entrance to the deletion loop —
//! asserting any other variant is therefore a complete proof that no HTTP
//! request, and no removal, can follow. Every fail-safe test below asserts that
//! variant rather than mocking a daemon and checking it was not called.
//! What: grammar (`glob_parse_*`), match plan (`glob_plan_*`), the destructive
//! decision (`glob_decide_*`), the confirmation classifier (`glob_confirm_*`),
//! and the report (`glob_report_*`).

use trusty_mpm::client::ManagedSessionSummary;

use super::{
    GlobDeleteOutcome, GlobDeleteRequest, GlobSkip, build_plan, confirm_matches_count,
    decide_glob_delete, parse_glob_delete, plan_lines,
};

/// Minimal `ManagedSessionSummary` fixture.
fn session(name: &str, state: &str) -> ManagedSessionSummary {
    ManagedSessionSummary {
        id: format!("{name}-id"),
        name: name.to_string(),
        state: state.to_string(),
        persisted_state: None,
        workspace_path: None,
        repo_url: None,
        branch: None,
        created_at: None,
        last_activity_at: None,
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
        stale_assets_unchecked: false,
        attached: false,
        slot: 1,
        deleted: false,
        auto_resume_parked: None,
    }
}

/// A tombstoned row — the daemon reports the slot already deleted (#3034).
fn tombstone(name: &str) -> ManagedSessionSummary {
    ManagedSessionSummary {
        deleted: true,
        ..session(name, "stopped")
    }
}

/// The standing fixture: three deletable test sessions, one live session, one
/// tombstone. Modelled on real names off `tm sessions ls` (`tm-tmpsywch8`,
/// `tm-apex-02`, …) so the patterns under test are the ones an operator types.
fn fleet() -> Vec<ManagedSessionSummary> {
    vec![
        session("tm-test-01", "stopped"),
        session("tm-test-02", "errored"),
        session("tm-test-03", "active"),
        session("tm-apex-02", "stopped"),
        tombstone("tm-test-04"),
    ]
}

/// Build a request the way the picker would, skipping the grammar.
fn req(pattern: &str, dry_run: bool) -> GlobDeleteRequest {
    GlobDeleteRequest {
        pattern: pattern.to_string(),
        dry_run,
    }
}

// ── grammar ─────────────────────────────────────────────────────────────────

#[test]
fn glob_parse_requires_metacharacter() {
    // The whole point of the metacharacter gate: `d` + a typo must stay inert.
    // `delete` reaching this function as `elete` must NOT become a bulk action.
    assert_eq!(parse_glob_delete("elete"), None);
    assert_eq!(parse_glob_delete("tm-test-01"), None);
    assert_eq!(parse_glob_delete("2"), None);
    // Each metacharacter opts in.
    assert_eq!(
        parse_glob_delete("tm-test-*"),
        Some(req("tm-test-*", false))
    );
    assert_eq!(
        parse_glob_delete("tm-test-0?"),
        Some(req("tm-test-0?", false))
    );
    assert_eq!(
        parse_glob_delete("tm-test-[12]"),
        Some(req("tm-test-[12]", false))
    );
}

#[test]
fn glob_parse_rejects_empty() {
    assert_eq!(parse_glob_delete(""), None);
    assert_eq!(parse_glob_delete("   "), None);
    // `--dry-run` with no pattern is not a pattern.
    assert_eq!(parse_glob_delete("--dry-run"), None);
}

#[test]
fn glob_parse_strips_dry_run() {
    assert_eq!(
        parse_glob_delete("tm-test-* --dry-run"),
        Some(req("tm-test-*", true))
    );
    assert_eq!(
        parse_glob_delete("  tm-test-*   --dry-run  "),
        Some(req("tm-test-*", true))
    );
}

#[test]
fn glob_parse_accepts_leading_dry_run() {
    // `d --dry-run tm-test-*` is the more common CLI habit; treating the flag
    // as glob text would report "no sessions match" exactly when the operator
    // asked to preview.
    assert_eq!(
        parse_glob_delete("--dry-run tm-test-*"),
        Some(req("tm-test-*", true))
    );
    assert_eq!(
        parse_glob_delete("  --dry-run   tm-test-*  "),
        Some(req("tm-test-*", true))
    );
    // The whitespace requirement keeps a `--dry-run`-prefixed NAME matchable.
    let parsed = parse_glob_delete("--dry-runner-*").expect("pattern");
    assert_eq!(parsed.pattern, "--dry-runner-*");
    assert!(!parsed.dry_run);
}

#[test]
fn glob_parse_keeps_inner_dry_run_text() {
    // Only a TRAILING `--dry-run` is the flag; mid-pattern it is glob text, so
    // a session actually named `tm--dry-run-01` stays reachable.
    let parsed = parse_glob_delete("tm--dry-run-*").expect("pattern");
    assert_eq!(parsed.pattern, "tm--dry-run-*");
    assert!(!parsed.dry_run);
}

// ── match plan ──────────────────────────────────────────────────────────────

fn plan_for(pattern: &str, sessions: &[ManagedSessionSummary]) -> super::GlobDeletePlan {
    plan_for_as(pattern, sessions, None)
}

/// Build a plan as if running inside the tmux session named `current`.
fn plan_for_as(
    pattern: &str,
    sessions: &[ManagedSessionSummary],
    current: Option<&str>,
) -> super::GlobDeletePlan {
    let glob = globset::GlobBuilder::new(pattern)
        .literal_separator(false)
        .case_insensitive(true)
        .build()
        .expect("valid glob");
    build_plan(sessions, &glob.compile_matcher(), current)
}

#[test]
fn glob_plan_splits_running_from_stopped() {
    let fleet = fleet();
    let plan = plan_for("tm-test-*", &fleet);
    // stopped + errored are deletable; active is kept.
    assert_eq!(plan.deletable, vec![0, 1]);
    assert_eq!(
        plan.skipped,
        vec![
            (2, GlobSkip::Running("active".to_string())),
            (4, GlobSkip::Tombstoned),
        ]
    );
    assert_eq!(plan.matched(), 4);
}

#[test]
fn glob_plan_skips_tombstoned_rows() {
    let fleet = fleet();
    let plan = plan_for("tm-test-04", &fleet);
    // Already deleted: matched, but never deleted a second time.
    assert!(plan.deletable.is_empty());
    assert_eq!(plan.skipped, vec![(4, GlobSkip::Tombstoned)]);
}

#[test]
fn glob_plan_empty_on_no_match() {
    let fleet = fleet();
    let plan = plan_for("tm-nothing-*", &fleet);
    assert_eq!(plan.matched(), 0);
}

#[test]
fn glob_plan_star_spans_separators() {
    // `literal_separator(false)`: `tm-*` must reach `tm-a-b`, not stop at `-`.
    let fleet = fleet();
    let plan = plan_for("tm-*", &fleet);
    assert_eq!(plan.matched(), 5, "every session starts with tm-");
}

#[test]
fn glob_plan_suffix_and_single_char_patterns() {
    let fleet = fleet();
    // `*-01` — the suffix form named in the request.
    assert_eq!(plan_for("*-01", &fleet).deletable, vec![0]);
    // `?` matches exactly one character.
    assert_eq!(plan_for("tm-test-0?", &fleet).matched(), 4);
    // A literal name still works when it happens to carry a metacharacter-free
    // sibling — checked through a bracket class so the grammar admits it.
    assert_eq!(plan_for("tm-apex-[0-9]2", &fleet).deletable, vec![3]);
}

#[test]
fn glob_plan_nameless_session_never_matches() {
    // The case the tmux-name ruling newly makes possible to get wrong: a record
    // with no tmux name must not be swept up by `*`. Treating an absent name as
    // an empty string would match every one of these.
    let sessions = vec![
        session("", "stopped"),
        session("   ", "stopped"),
        session("tm-test-01", "stopped"),
    ];
    let plan = plan_for("*", &sessions);
    assert_eq!(
        plan.deletable,
        vec![2],
        "only the named session is deletable under `*`"
    );
    assert_eq!(
        plan.matched(),
        1,
        "a nameless session must not appear in the match set at all"
    );
    // And the same holds for a prefix pattern, which an empty name would also
    // fail — belt and braces on the one that would actually match.
    assert_eq!(plan_for("*", &sessions[..2]).matched(), 0);
}

#[test]
fn glob_plan_excludes_current_session() {
    // The self-guard, keyed on the tmux name. `tm-test-01` is stopped — so the
    // running-session guard would NOT have saved it — and it is still kept.
    let plan = plan_for_as("tm-test-*", &fleet(), Some("tm-test-01"));
    assert!(
        !plan.deletable.contains(&0),
        "the caller's own session must never be bulk-deleted"
    );
    assert!(plan.skipped.contains(&(0, GlobSkip::SelfSession)));
    // The rest of the batch is unaffected.
    assert_eq!(plan.deletable, vec![1]);
}

#[test]
fn glob_plan_not_in_tmux_still_guards_running() {
    // Outside tmux there is no own-session to name, so `current` is `None`.
    // That disables ONLY the self-check — the running-session guard still keeps
    // every live session out of the batch.
    let plan = plan_for_as("tm-test-*", &fleet(), None);
    assert!(
        !plan.deletable.contains(&2),
        "the active session is kept even with no tmux identity available"
    );
    assert!(
        plan.skipped
            .iter()
            .any(|(i, r)| *i == 2 && matches!(r, GlobSkip::Running(s) if s == "active"))
    );
}

// ── the destructive decision (fail-safe proofs) ─────────────────────────────

#[test]
fn glob_decide_no_match_is_not_destructive() {
    // Zero matches must be a report, not a silent success — and above all not
    // a path to the deletion loop.
    let (outcome, plan) = decide_glob_delete(&fleet(), &req("tm-nothing-*", false), None);
    assert_eq!(outcome, GlobDeleteOutcome::NoMatch);
    assert!(plan.deletable.is_empty());
    assert_ne!(outcome, GlobDeleteOutcome::Confirm);
}

#[test]
fn glob_decide_all_running_is_not_destructive() {
    // Every match is live: nothing is deletable, so the driver returns before
    // the prompt. This is the guard that structurally excludes the caller's own
    // session — an attached session is `active`, therefore never bulk-deleted.
    let live = vec![
        session("tm-test-01", "active"),
        session("tm-test-02", "attached"),
        session("tm-test-03", "provisioning"),
    ];
    let (outcome, plan) = decide_glob_delete(&live, &req("tm-test-*", false), None);
    assert_eq!(outcome, GlobDeleteOutcome::NothingDeletable);
    assert!(
        plan.deletable.is_empty(),
        "a running session must never enter the bulk delete set"
    );
    assert_eq!(plan.skipped.len(), 3);
}

#[test]
fn glob_decide_match_all_never_reaches_running_sessions() {
    // `*` — the widest possible pattern, and the one the request calls out.
    // It still cannot touch a live session; only the two stopped/errored rows
    // are deletable, and the operator must type `2` to confirm.
    let (outcome, plan) = decide_glob_delete(&fleet(), &req("*", false), None);
    assert_eq!(outcome, GlobDeleteOutcome::Confirm);
    assert_eq!(plan.matched(), 5, "`*` matches the whole fleet");
    assert_eq!(
        plan.deletable,
        vec![0, 1, 3],
        "only stopped/errored rows are deletable under `*`"
    );
    // Proof the live session survives `*`.
    assert!(
        plan.skipped
            .iter()
            .any(|(i, r)| *i == 2 && matches!(r, GlobSkip::Running(s) if s == "active"))
    );
    // And the confirmation for `*` is the count, which will not be `y`.
    assert!(!confirm_matches_count("y", plan.deletable.len()));
}

#[test]
fn glob_decide_dry_run_never_confirms() {
    // Same pattern, same matches — but `--dry-run` short-circuits ahead of the
    // prompt, so the deletion loop is unreachable.
    let (wet, _) = decide_glob_delete(&fleet(), &req("tm-test-*", false), None);
    let (dry, plan) = decide_glob_delete(&fleet(), &req("tm-test-*", true), None);
    assert_eq!(wet, GlobDeleteOutcome::Confirm);
    assert_eq!(dry, GlobDeleteOutcome::DryRun);
    assert_ne!(dry, GlobDeleteOutcome::Confirm);
    assert_eq!(plan.deletable.len(), 2, "the plan is still computed");
}

#[test]
fn glob_decide_invalid_pattern() {
    // An unclosed class must report the compiler's message and delete nothing.
    let (outcome, plan) = decide_glob_delete(&fleet(), &req("tm-test-[", false), None);
    assert!(matches!(outcome, GlobDeleteOutcome::InvalidPattern(_)));
    assert_ne!(outcome, GlobDeleteOutcome::Confirm);
    assert!(plan.deletable.is_empty());
}

#[test]
fn glob_decide_confirm_on_deletable() {
    let (outcome, plan) = decide_glob_delete(&fleet(), &req("tm-test-*", false), None);
    assert_eq!(outcome, GlobDeleteOutcome::Confirm);
    assert_eq!(plan.deletable, vec![0, 1]);
}

#[test]
fn glob_decide_literal_name_pattern() {
    // A pattern with a metacharacter that resolves to exactly one session —
    // the narrow end of the feature, still confirmed like any other.
    let (outcome, plan) = decide_glob_delete(&fleet(), &req("tm-apex-0?", false), None);
    assert_eq!(outcome, GlobDeleteOutcome::Confirm);
    assert_eq!(plan.deletable, vec![3]);
}

// ── confirmation ────────────────────────────────────────────────────────────

#[test]
fn glob_confirm_requires_exact_count() {
    assert!(confirm_matches_count("2", 2));
    assert!(confirm_matches_count("  2  ", 2));
    // A number that is not THE number is a cancel — this is what catches an
    // operator confirming a count they did not actually read.
    assert!(!confirm_matches_count("1", 2));
    assert!(!confirm_matches_count("3", 2));
    assert!(!confirm_matches_count("20", 2));
}

#[test]
fn glob_confirm_rejects_yes() {
    // The reflex answers must not confirm a bulk deletion.
    for line in ["y", "Y", "yes", "YES", "", "  ", "all", "force", "n"] {
        assert!(
            !confirm_matches_count(line, 3),
            "'{line}' must not confirm a bulk delete"
        );
    }
}

// ── report ──────────────────────────────────────────────────────────────────

#[test]
fn glob_decide_star_never_deletes_nameless_sessions() {
    // End-to-end through the destructive decision: `*` against a fleet that is
    // ENTIRELY nameless must report NoMatch, never Confirm.
    let nameless = vec![session("", "stopped"), session("  ", "errored")];
    let (outcome, plan) = decide_glob_delete(&nameless, &req("*", false), None);
    assert_eq!(outcome, GlobDeleteOutcome::NoMatch);
    assert_ne!(
        outcome,
        GlobDeleteOutcome::Confirm,
        "`*` must not reach the deletion loop via nameless sessions"
    );
    assert!(plan.deletable.is_empty());
}

#[test]
fn glob_decide_self_only_match_is_not_destructive() {
    // A pattern whose only match is the caller's own session deletes nothing.
    let mine = vec![session("tm-mine-01", "stopped")];
    let (outcome, plan) = decide_glob_delete(&mine, &req("tm-mine-*", false), Some("tm-mine-01"));
    assert_eq!(outcome, GlobDeleteOutcome::NothingDeletable);
    assert_ne!(outcome, GlobDeleteOutcome::Confirm);
    assert!(plan.deletable.is_empty());
}

#[test]
fn glob_report_disambiguates_same_name() {
    // tmux names get reused, so a match set can hold two rows with the same
    // name. The listing must still tell them apart.
    let dupes = vec![
        ManagedSessionSummary {
            id: "aaaaaaaa-1111".to_string(),
            ..session("tm-dogfood", "stopped")
        },
        ManagedSessionSummary {
            id: "bbbbbbbb-2222".to_string(),
            ..session("tm-dogfood", "stopped")
        },
    ];
    let (_, plan) = decide_glob_delete(&dupes, &req("tm-dogfood*", false), None);
    assert_eq!(plan.deletable.len(), 2);
    let lines = plan_lines(&dupes, &plan);
    assert_ne!(
        lines[0], lines[1],
        "two same-named sessions must not render as identical rows"
    );
    assert!(lines[0].contains("aaaaaaaa"));
    assert!(lines[1].contains("bbbbbbbb"));
}

#[test]
fn glob_report_leads_with_the_tmux_name() {
    // The operator matched on the tmux name, so the row must lead with it.
    let fleet = fleet();
    let (_, plan) = decide_glob_delete(&fleet, &req("tm-apex-*", false), None);
    let lines = plan_lines(&fleet, &plan);
    assert_eq!(lines.len(), 1);
    assert!(
        lines[0].trim_start().starts_with("DELETE  tm-apex-02"),
        "row must lead with the marker then the tmux name, got: {}",
        lines[0]
    );
}

/// The count the prompt accepts is exactly the count of `DELETE` rows shown.
///
/// Why this test carries the weight it does: the prompt deliberately does not
/// print the number to type, so the operator derives it by counting `DELETE`
/// markers in the listing. That makes "markers shown" and "count accepted" two
/// independently-computed values — `plan_lines` walks the plan to render, and
/// `confirm_matches_count` compares against `plan.deletable.len()`. If they ever
/// diverge, a correct operator who counts carefully is REJECTED, and the
/// obvious repair is to print the number again, which is exactly the property
/// the review said must not be given up. Nothing else in the suite couples the
/// two.
#[test]
fn glob_report_marks_delete_rows() {
    // Several shapes, so a divergence cannot hide in one arrangement: a mixed
    // plan, an all-deletable plan, and the widest pattern.
    for pattern in ["tm-test-*", "tm-apex-*", "*"] {
        let fleet = fleet();
        let (_, plan) = decide_glob_delete(&fleet, &req(pattern, false), None);
        let lines = plan_lines(&fleet, &plan);

        let marked = lines
            .iter()
            .filter(|l| l.trim_start().starts_with("DELETE"))
            .count();
        let kept = lines
            .iter()
            .filter(|l| l.trim_start().starts_with("keep"))
            .count();

        // The operator's count of DELETE rows is what the prompt must accept.
        assert!(
            confirm_matches_count(&marked.to_string(), plan.deletable.len()),
            "'{pattern}': counting {marked} DELETE row(s) must be accepted as \
             the confirmation, but the prompt expects {}",
            plan.deletable.len()
        );
        // Stated the other way round, so a plan that renders too FEW markers
        // fails too — not just one that renders too many.
        assert_eq!(
            marked,
            plan.deletable.len(),
            "'{pattern}': DELETE markers shown must equal the deletable count"
        );
        // Every row is accounted for, so counting cannot silently skip one.
        assert_eq!(marked + kept, lines.len());
        assert_eq!(kept, plan.skipped.len());

        // A miscount is rejected in both directions — the coupling is exact,
        // not merely "some number works".
        assert!(!confirm_matches_count(
            &(marked + 1).to_string(),
            plan.deletable.len()
        ));
        if marked > 0 {
            assert!(!confirm_matches_count(
                &(marked - 1).to_string(),
                plan.deletable.len()
            ));
        }
    }
}

#[test]
fn glob_report_lists_every_match() {
    let fleet = fleet();
    let (_, plan) = decide_glob_delete(&fleet, &req("tm-test-*", false), None);
    let lines = plan_lines(&fleet, &plan);
    assert_eq!(
        lines.len(),
        plan.matched(),
        "every matched row is shown, protected ones included"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("tm-test-01") && l.contains("stopped"))
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("tm-test-03") && l.contains("keep") && l.contains("active"))
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("tm-test-04") && l.contains("already deleted"))
    );
}
