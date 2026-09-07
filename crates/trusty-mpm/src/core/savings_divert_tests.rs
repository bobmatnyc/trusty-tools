//! Tests for the bulk-read diversion savings producer (#6959).
//!
//! Why: the producer's whole contract is "claim only what the diversion
//! genuinely avoided". A permissive implementation — one that ignored the
//! worker's own cost, or priced an unknown model at a guessed rate — would put a
//! fabricated figure on the operator's status bar. The fail-open write branch
//! needs its own test for the opposite reason: a ledger that cannot be written
//! must not turn a diversion that answered into one that failed.
//! What: the row builder is driven with hand-supplied byte counts and an
//! injected price, so every decline branch is asserted with no configured model;
//! the write path is driven against a tempdir.
//! Test: this file.

use super::*;

use crate::core::savings::{fold_session, savings_log_in};

/// A price stand-in: Sonnet's published input rate, as the shared table carries
/// it. Used so the arithmetic below is hand-checkable.
fn sonnet_price() -> Option<ParentModel> {
    Some(ParentModel {
        id: "claude-sonnet-4-6".to_string(),
        input_per_million: 3.0,
        source: MODEL_SOURCE_STATUSLINE,
    })
}

/// The real price lookup, pinned to a Fable parent session.
///
/// Why (#6972): this is `resolve_session_price`'s tail with the model fixed
/// instead of resolved from the three sources, so the test exercises the shared
/// table rather than a stand-in. Before #6972 the table returned `None` for this
/// slug and the closure declined, which is exactly what the test below rejects.
/// What: `claude-fable-5-1` and its table input rate, attributed to the
/// statusline record — the source a real Fable session prices from.
/// Test: `a_fable_parent_session_produces_a_priced_row`.
fn fable_price() -> Option<ParentModel> {
    let model = "claude-fable-5-1".to_string();
    let pricing = trusty_common::inference::pricing(&model)?;
    Some(ParentModel {
        input_per_million: pricing.input,
        id: model,
        source: MODEL_SOURCE_STATUSLINE,
    })
}

/// Why (#6959 acceptance criterion 1): the hand-computed delta is the whole
/// claim. 400,000 file bytes are 100,000 tokens at four bytes each; a 4,000-byte
/// summary is 1,000; the delta is 99,000 tokens, which at Sonnet's $3/Mtok input
/// rate is $0.297, less the worker's own $0.02 — $0.277.
/// Test: itself.
#[test]
fn a_hand_computed_delta_matches_the_row() {
    let row = divert_row("sess-a", 400_000, 4_000, 0.02, sonnet_price).expect("a row");
    assert_eq!(row.session_id, "sess-a");
    assert_eq!(row.tokens_saved, 99_000);
    assert!(
        (row.cost_saved_usd - 0.277).abs() < 1e-9,
        "cost was {}",
        row.cost_saved_usd
    );
    assert!(
        row.basis.contains("100000") && row.basis.contains("1000") && row.basis.contains("0.02"),
        "the basis must carry file tokens, summary tokens, and the worker cost: {}",
        row.basis
    );
}

/// Why (#6959 acceptance criterion 2): a summary at least as large as the files
/// it summarises saved nothing. Writing a row there would state a saving that
/// did not happen.
/// Test: itself.
#[test]
fn no_row_when_the_summary_is_not_smaller_than_the_files() {
    assert!(
        divert_row("sess-a", 4_000, 8_000, 0.0, sonnet_price).is_none(),
        "a summary larger than the files must write no row"
    );
    assert!(
        divert_row("sess-a", 4_000, 4_000, 0.0, sonnet_price).is_none(),
        "an equal-size summary must write no row"
    );
    // Rounding must not manufacture a delta out of a sub-token difference.
    assert!(
        divert_row("sess-a", 4_003, 4_000, 0.0, sonnet_price).is_none(),
        "a sub-token delta must write no row"
    );
}

/// Why (#6959 acceptance criterion 3): the fold reads `technique` for a
/// per-technique breakdown, so the exact string is part of the contract.
/// Test: itself.
#[test]
fn divert_row_carries_the_named_technique() {
    let row = divert_row("sess-a", 400_000, 4_000, 0.02, sonnet_price).expect("a row");
    assert_eq!(row.technique, TECHNIQUE_DIVERT);
    assert_eq!(row.technique, "divert");
}

/// Why (#6959): a model the shared table does not know must decline the row,
/// never substitute a guessed rate. A fabricated price is worse than no figure —
/// it is a figure the operator would act on.
/// Test: itself.
#[test]
fn no_row_when_the_parent_model_cannot_be_priced() {
    assert!(
        divert_row("sess-a", 400_000, 4_000, 0.02, || None).is_none(),
        "an unpriceable parent model must write no row"
    );
    assert!(
        divert_row("sess-a", 400_000, 4_000, 0.02, || Some(ParentModel {
            id: "m".to_string(),
            input_per_million: 0.0,
            source: MODEL_SOURCE_CONFIG_FALLBACK,
        }))
        .is_none(),
        "a zero rate must write no row"
    );
}

/// Why (#6972): the owner's daily-driver parent session runs on
/// `claude-fable-5-1`, and that slug carries none of the `opus`/`sonnet`/`haiku`
/// family words the shared table matched on. It therefore priced as `None`, the
/// producer took its "unpriceable parent model" decline branch, and the session
/// that generates the most diversions recorded no savings at all — which is the
/// one case the statusline segment exists to show. Driving the REAL table
/// (rather than a stand-in) is what makes this test fail on a table without the
/// Fable arm.
/// Test: itself.
#[test]
fn a_fable_parent_session_produces_a_priced_row() {
    let row = divert_row("sess-fable", 400_000, 4_000, 0.02, fable_price)
        .expect("a Fable parent session must produce a priced row, not a decline");
    assert_eq!(row.tokens_saved, 99_000);
    // 99,000 tokens at Fable's $10/Mtok input rate is $0.99, less the worker's
    // own $0.02 — $0.97.
    assert!(
        (row.cost_saved_usd - 0.97).abs() < 1e-9,
        "cost was {}",
        row.cost_saved_usd
    );
    assert!(
        row.basis.contains("claude-fable-5-1"),
        "the basis must name the parent model it priced at: {}",
        row.basis
    );
    assert_eq!(row.model_source, MODEL_SOURCE_STATUSLINE);

    // And the written row comes back out of the fold the statusline reads with.
    let dir = tempfile::tempdir().expect("temp dir");
    record_divert_with(dir.path(), "sess-fable", 400_000, 4_000, 0.02, fable_price);
    let ledger = savings_log_in(dir.path());
    let total = fold_session(&ledger, "sess-fable");
    assert_eq!(total.rows, 1, "the Fable diversion must fold as one row");
    assert!(
        (total.cost_saved_usd - 0.97).abs() < 1e-9,
        "folded cost was {}",
        total.cost_saved_usd
    );
}

/// Why (#6959): the worker is not free. A diversion whose worker billed more
/// than the avoided tokens were worth cost money rather than saving it, and the
/// row must not claim the gross figure as if the worker were free. The
/// exactly-cancelling case is the sharp one: the subtraction leaves float
/// residue near `4e-17`, so an implementation testing `> 0.0` writes a row
/// claiming 99,000 tokens saved for no money at all.
/// Test: itself.
#[test]
fn no_row_when_the_worker_cost_exceeds_the_saving() {
    // 400,000 B is 100,000 tokens; less a 4,000 B summary that is $0.297 gross.
    assert!(
        divert_row("sess-a", 400_000, 4_000, 0.297, sonnet_price).is_none(),
        "a worker that cost exactly the delta must write no row"
    );
    assert!(
        divert_row(
            "sess-a",
            400_000,
            4_000,
            0.297 - MIN_COST_SAVED_USD / 2.0,
            sonnet_price
        )
        .is_none(),
        "a sub-micro-dollar saving must write no row"
    );
    assert!(
        divert_row("sess-a", 400_000, 4_000, 5.0, sonnet_price).is_none(),
        "a worker that cost more than the delta must write no row"
    );
}

/// Why (#6959): the row is attributed to a session, and the statusline folds by
/// session id. A row written under no id is unattributable and would inflate
/// nothing while cluttering the ledger.
/// Test: itself.
#[test]
fn no_row_without_a_session_id() {
    let dir = tempfile::tempdir().expect("temp dir");
    let ledger = savings_log_in(dir.path());
    record_divert_with(dir.path(), "  ", 400_000, 4_000, 0.02, sonnet_price);
    assert!(
        !ledger.exists(),
        "an empty session id must not create a ledger"
    );
}

/// Why (#6959, deliverable 4): the end of the chain — a recorded diversion has
/// to come back out of the fold under the same session id the statusline reads
/// with, or the segment stays absent no matter what was written.
/// Test: itself.
#[test]
fn a_recorded_diversion_folds_into_the_session_total() {
    let dir = tempfile::tempdir().expect("temp dir");
    let ledger = savings_log_in(dir.path());
    record_divert_with(dir.path(), "sess-a", 400_000, 4_000, 0.02, sonnet_price);

    let total = fold_session(&ledger, "sess-a");
    assert_eq!(total.rows, 1);
    assert_eq!(total.tokens_saved, 99_000);
    assert!((total.cost_saved_usd - 0.277).abs() < 1e-9);
    // Another session's status bar reads nothing from the same file.
    assert_eq!(fold_session(&ledger, "sess-b").rows, 0);
}

/// Why (#6959, deliverable 3): this is a FAIL-OPEN branch, and fail-open code
/// only stays fail-open if something asserts it. The answer is already on the
/// agent's stdout when this runs, so a ledger that cannot be written must cost
/// the row and nothing else. An implementation that `?`-propagated or
/// `expect()`-ed the write would fail this test by panicking.
/// Test: itself.
#[test]
fn a_ledger_write_failure_does_not_fail_the_diversion() {
    let dir = tempfile::tempdir().expect("temp dir");
    // A regular file where the `usage/` directory must go: `create_dir_all`
    // inside the writer cannot succeed, so the append returns an IO error.
    let blocker = dir.path().join("usage");
    std::fs::write(&blocker, "not a directory").expect("write blocker");
    let ledger = savings_log_in(dir.path());

    record_divert_with(dir.path(), "sess-a", 400_000, 4_000, 0.02, sonnet_price);

    assert!(
        !ledger.exists(),
        "the unwritable ledger must stay unwritten, not partially created"
    );
    assert!(
        blocker.is_file(),
        "the blocking file must be left exactly as it was"
    );
}

/// Why: the price the producer uses must come from the shared table, not a
/// fourth copy. Asserting the resolved rate against a direct table lookup pins
/// that without depending on which model this machine is configured for.
/// Test: itself.
#[test]
fn resolve_session_price_agrees_with_the_shared_table() {
    let dir = tempfile::tempdir().expect("temp dir");
    // A machine configured for a model outside the table resolves to `None`,
    // which declines the row — the documented behaviour, not a test failure.
    if let Some(parent) = resolve_session_price(dir.path(), "sess-a") {
        let table = trusty_common::inference::pricing(&parent.id)
            .expect("the resolved model must be one the table knows");
        assert_eq!(
            parent.input_per_million, table.input,
            "the rate must be the table's input rate"
        );
        assert!(
            parent.input_per_million > 0.0,
            "a priced model must have a positive input rate"
        );
    }
}

/// Why (#6972): this is the precedence the issue is about, driven with every
/// combination of the three sources present and absent. The third row is the
/// bug itself — a session running Opus, no pinned `ANTHROPIC_MODEL`, and a
/// config chain that answers Sonnet. Before #6972 that resolved Sonnet and
/// priced the row at a fifth of what the diversion saved.
/// Test: itself.
#[test]
fn parent_model_precedence_table() {
    let opus = "claude-opus-4-5";
    let haiku = "claude-haiku-4-5";
    let sonnet = "claude-sonnet-4-6";

    // (env, statusline, expected model, expected source)
    let cases: &[(Option<&str>, Option<&str>, &str, &str)] = &[
        // All three present: the operator's explicit pin wins.
        (Some(haiku), Some(opus), haiku, MODEL_SOURCE_ENV),
        // Env only.
        (Some(haiku), None, haiku, MODEL_SOURCE_ENV),
        // THE BUG: statusline says opus, env unset, config says sonnet.
        (None, Some(opus), opus, MODEL_SOURCE_STATUSLINE),
        // Neither: the config chain always answers, and it is a guess.
        (None, None, sonnet, MODEL_SOURCE_CONFIG_FALLBACK),
        // A blank source is an absent one, not an empty model id.
        (Some(""), Some(opus), opus, MODEL_SOURCE_STATUSLINE),
        (
            Some("  "),
            Some("   "),
            sonnet,
            MODEL_SOURCE_CONFIG_FALLBACK,
        ),
        // Surrounding whitespace is trimmed off whichever source answers.
        (Some(" claude-haiku-4-5 "), None, haiku, MODEL_SOURCE_ENV),
    ];

    for (env, statusline, want_model, want_source) in cases {
        let (model, source) = choose_parent_model(
            env.map(str::to_string),
            statusline.map(str::to_string),
            || sonnet.to_string(),
        );
        assert_eq!(
            (model.as_str(), source),
            (*want_model, *want_source),
            "env={env:?} statusline={statusline:?}"
        );
    }
}

/// Why (#6972): the three precedence rungs, each asserted on its own so a
/// failure names which rung broke rather than only that the table disagreed.
/// Test: itself.
#[test]
fn env_wins_over_every_other_source() {
    let (model, source) = choose_parent_model(
        Some("claude-haiku-4-5".to_string()),
        Some("claude-opus-4-5".to_string()),
        || panic!("the config chain must not be consulted when a better source answered"),
    );
    assert_eq!(model, "claude-haiku-4-5");
    assert_eq!(source, MODEL_SOURCE_ENV);
}

/// Why (#6972 closure condition 1): the record the statusline hook wrote is the
/// only source carrying the model Claude Code is actually running, so it must
/// beat the config chain's default.
/// Test: itself.
#[test]
fn the_statusline_record_outranks_the_config_chain() {
    let (model, source) = choose_parent_model(None, Some("claude-opus-4-5".to_string()), || {
        panic!("the config chain must not be consulted when the record answered")
    });
    assert_eq!(model, "claude-opus-4-5");
    assert_eq!(source, MODEL_SOURCE_STATUSLINE);
}

/// Why (#6972 closure condition 2): before the first render there is no record,
/// and behaviour must be unchanged from #6971 — the config chain answers. What
/// changes is that the row says so.
/// Test: itself.
#[test]
fn the_config_chain_is_the_last_resort() {
    let (model, source) = choose_parent_model(None, None, || "claude-sonnet-4-6".to_string());
    assert_eq!(model, "claude-sonnet-4-6");
    assert_eq!(source, MODEL_SOURCE_CONFIG_FALLBACK);
}

/// Why (#6972): an unresolvable model must not price as Sonnet in silence. The
/// row carries the source it was priced from, so an operator reading the ledger
/// can tell a measured price from a guessed one without reproducing the machine.
/// Test: itself.
#[test]
fn divert_row_names_the_model_source() {
    let guessed = divert_row("sess-a", 400_000, 4_000, 0.02, || {
        Some(ParentModel {
            id: "claude-sonnet-4-6".to_string(),
            input_per_million: 3.0,
            source: MODEL_SOURCE_CONFIG_FALLBACK,
        })
    })
    .expect("a row");
    assert_eq!(guessed.model_source, MODEL_SOURCE_CONFIG_FALLBACK);
    assert_eq!(guessed.model_source, "config-fallback");
    assert!(
        guessed.basis.contains("config-fallback"),
        "the basis must name the guessed source too: {}",
        guessed.basis
    );

    // A measured price is labelled differently, so the two are distinguishable.
    let measured = divert_row("sess-a", 400_000, 4_000, 0.02, sonnet_price).expect("a row");
    assert_eq!(measured.model_source, MODEL_SOURCE_STATUSLINE);
    assert_ne!(measured.model_source, guessed.model_source);
}

/// Why (#6972 closure condition 1, end to end): the render writes the record,
/// the diversion reads it, and the row that lands in the ledger is priced at
/// Opus rather than the config chain's Sonnet. This is the whole issue in one
/// assertion — 99,000 tokens at $15/Mtok is $1.485, less the worker's $0.02.
/// Test: itself.
#[test]
fn a_recorded_opus_session_prices_its_diversion_at_opus() {
    let dir = tempfile::tempdir().expect("temp dir");
    crate::core::session_model::record_session_model(dir.path(), "sess-a", "claude-opus-4-5");

    let parent = resolve_session_price(dir.path(), "sess-a").expect("opus must price");
    assert_eq!(parent.id, "claude-opus-4-5");
    assert_eq!(parent.input_per_million, 15.0);
    assert_eq!(parent.source, MODEL_SOURCE_STATUSLINE);

    record_divert(dir.path(), "sess-a", 400_000, 4_000, 0.02);
    let total = fold_session(&savings_log_in(dir.path()), "sess-a");
    assert_eq!(total.rows, 1);
    assert!(
        (total.cost_saved_usd - 1.465).abs() < 1e-9,
        "the row must be priced at Opus, not the config chain: {}",
        total.cost_saved_usd
    );
}
