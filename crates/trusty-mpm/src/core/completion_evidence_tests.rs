//! Tests for the completion-standard check (#10575), fixtures F3 and F4.

use crate::core::completion_evidence::{claims_completion, warnings_for};

const FULL_MAP: &str = "Done: the scenarios are goals only.\n\n## Evidence map\n\
- Verified: S1-S24 read back after the last write\n\
- Helper-reported: lint pass (agent)\n\
- Unfinished: none\n\
- Untested: none\n";

fn codes(message: &str) -> Vec<String> {
    warnings_for(message)
        .into_iter()
        .map(|w| w.split(':').next().unwrap_or_default().to_string())
        .collect()
}

#[test]
fn a_done_claim_without_a_map_warns() {
    assert_eq!(
        codes("Done. Goals only applied; the file is 18% shorter."),
        ["MISSING_EVIDENCE_MAP"]
    );
}

#[test]
fn a_full_map_draws_no_warning() {
    assert!(
        warnings_for(FULL_MAP).is_empty(),
        "{:?}",
        warnings_for(FULL_MAP)
    );
}

#[test]
fn a_map_missing_a_category_warns() {
    let msg = "Completed.\n\nEvidence map:\n- Verified: S1\n- Unfinished: none\n";
    let w = warnings_for(msg);
    assert_eq!(codes(msg), ["EVIDENCE_MAP_INCOMPLETE"]);
    assert!(w[0].contains("helper-reported / untested"), "{w:?}");
}

#[test]
fn a_negated_claim_is_not_a_completion_claim() {
    // F3 honest-incomplete variant.
    let msg = "Not finished: 12 of 24 rewritten, none checked yet.";
    assert!(!claims_completion(msg));
    assert!(warnings_for(msg).is_empty());
}

#[test]
fn a_message_with_no_claim_draws_no_warning() {
    assert!(warnings_for("Reading the scenarios file now.").is_empty());
}

#[test]
fn a_claim_inside_a_fence_is_ignored() {
    assert!(warnings_for("```\ntest result: ok. done\n```\n").is_empty());
}

#[test]
fn an_invented_target_warns() {
    // F4: "cut to 600 words" with no owner quote.
    assert_eq!(
        codes("Brief: cut docs/working-rules.md to 600 words."),
        ["INVENTED_TARGET"]
    );
}

#[test]
fn an_owner_quoted_limit_passes() {
    let msg =
        "Keep it under 1 page.\nOWNER-REQUESTED-LIMIT: \"keep the one-page summary to one page\"";
    assert!(warnings_for(msg).is_empty(), "{:?}", warnings_for(msg));
}

#[test]
fn an_owner_limit_without_a_quote_warns() {
    assert_eq!(
        codes("Trim to 600 words.\nOWNER-REQUESTED-LIMIT: shorter please"),
        ["OWNER-REQUESTED-LIMIT"]
    );
}

#[test]
fn a_percent_target_warns() {
    assert_eq!(codes("Plan: cut it by 20%."), ["INVENTED_TARGET"]);
}

#[test]
fn contracted_and_yet_negations_are_not_claims() {
    for msg in [
        "The rewrite isn't done.",
        "The agent hasn't finished.",
        "Section 3 is not yet done.",
        "The migration is not finished.",
    ] {
        assert!(!claims_completion(msg), "{msg}");
    }
}

#[test]
fn unverified_does_not_satisfy_the_verified_category() {
    let msg = "Done.\n\nEvidence map:\n- Unverified: S1\n- Helper-reported: lint\n\
               - Unfinished: none\n- Untested: none\n";
    assert_eq!(codes(msg), ["EVIDENCE_MAP_INCOMPLETE"]);
    let spaced = FULL_MAP.replace("Helper-reported", "Helper reported");
    assert!(
        warnings_for(&spaced).is_empty(),
        "{:?}",
        warnings_for(&spaced)
    );
}

#[test]
fn a_bold_owner_limit_with_a_trailing_period_passes() {
    for line in [
        "**OWNER-REQUESTED-LIMIT:** \"one page\".",
        "**OWNER-REQUESTED-LIMIT**: \"one page\"",
    ] {
        let msg = format!("Keep it under 1 page.\n{line}");
        assert!(
            warnings_for(&msg).is_empty(),
            "{line}: {:?}",
            warnings_for(&msg)
        );
    }
}
