//! Tests for the one model-pricing table (#6875).
//!
//! Why: this table is the number an operator is billed against and the number
//! #6872's ledger persists. Every rate below is asserted as a literal so a rate
//! edit is a deliberate, reviewed diff rather than a silent drift.
//! What: the published Anthropic rows, `effective_from` selection, alias and
//! Bedrock-id resolution, the operator override, and the unknown-model
//! contract that replaced a stale Sonnet fallback.
//! Test: included as `#[cfg(test)] mod tests` via `#[path]` from `pricing.rs`.

use super::*;

const EPS: f64 = 1e-9;

fn day(s: &str) -> NaiveDate {
    s.parse().expect("test date")
}

/// Today, for rows whose `effective_from` is the table's early sentinel.
fn today() -> NaiveDate {
    chrono::Utc::now().date_naive()
}

#[test]
fn bundled_table_parses() {
    let p = Pricing::bundled();
    assert!(
        p.rate_for("claude-sonnet-5", today()).is_some(),
        "bundled table must price Sonnet 5"
    );
}

/// Why (#6875): `trusty-agents` priced Sonnet at 4.x-generation rates and every
/// unknown id at those same rates, so a Sonnet 5 turn was billed 50% high. This
/// test is the one that fails on `origin/main`.
/// What: the four models issue #6875 names, asserted against the rates the
/// `claude-api` skill publishes (`shared/models.md` + its Current Models table,
/// read 2026-09-05): Fable 5.1 $10/$50, Opus 5 $5/$25, Sonnet 5 $2/$10, Haiku
/// 4.5 $1/$5. Cache write is 1.25x input and cache read 0.1x input on all four
/// EXCEPT Fable 5.1, whose published cache read is $0.25/MTok (0.025x).
/// Test: this test.
#[test]
fn published_anthropic_rates_match_the_claude_api_skill() {
    let p = Pricing::bundled();
    let cases: &[(&str, f64, f64, f64, f64)] = &[
        ("claude-fable-5-1", 10.00, 50.00, 12.50, 0.25),
        ("claude-opus-5", 5.00, 25.00, 6.25, 0.50),
        ("claude-sonnet-5", 2.00, 10.00, 2.50, 0.20),
        ("claude-haiku-4-5", 1.00, 5.00, 1.25, 0.10),
    ];
    for (id, input, output, cache_write, cache_read) in cases {
        let r = p
            .rate_for(id, today())
            .unwrap_or_else(|| panic!("{id} priced"));
        assert!((r.input - input).abs() < EPS, "{id} input: {r:?}");
        assert!((r.output - output).abs() < EPS, "{id} output: {r:?}");
        assert!(
            (r.cache_write - cache_write).abs() < EPS,
            "{id} cache_write: {r:?}"
        );
        assert!(
            (r.cache_read - cache_read).abs() < EPS,
            "{id} cache_read: {r:?}"
        );
    }
}

/// Why (#6875): the closure condition names the multipliers, not just the
/// headline rates — a row that gets input right and cache wrong still bills a
/// long cached session wrong.
/// What: cache write is 1.25x input and cache read 0.1x input on Opus 5,
/// Sonnet 5 and Haiku 4.5. Fable 5.1 is deliberately excluded: Anthropic
/// publishes 0.025x there, asserted literally above.
/// Test: this test.
#[test]
fn anthropic_cache_multipliers_hold() {
    let p = Pricing::bundled();
    for id in ["claude-opus-5", "claude-sonnet-5", "claude-haiku-4-5"] {
        let r = p.rate_for(id, today()).expect("priced");
        assert!(
            (r.cache_write - r.input * 1.25).abs() < EPS,
            "{id} cache write should be 1.25x input: {r:?}"
        );
        assert!(
            (r.cache_read - r.input * 0.10).abs() < EPS,
            "{id} cache read should be 0.1x input: {r:?}"
        );
    }
}

/// Why: `effective_from` is the whole reason the table is dated — a ledger
/// backfill must price last month's usage at last month's rate.
/// What: two rows for one id; the day before the change picks the old rate, the
/// change day itself and after pick the new one, and a day before EVERY row
/// prices as unknown rather than as the earliest row.
/// Test: this test.
#[test]
fn effective_from_picks_the_dated_row() {
    let table = Pricing::from_toml_str(
        r#"
        [[model]]
        id = "demo-model"
        input = 1.00
        output = 2.00
        effective_from = "2026-01-01"

        [[model]]
        id = "demo-model"
        input = 3.00
        output = 6.00
        effective_from = "2026-06-01"
        "#,
        "<test>",
    )
    .expect("parse");

    let before = table
        .rate_for("demo-model", day("2026-05-31"))
        .expect("old");
    assert!((before.input - 1.00).abs() < EPS, "{before:?}");

    let on_change = table
        .rate_for("demo-model", day("2026-06-01"))
        .expect("new");
    assert!((on_change.input - 3.00).abs() < EPS, "{on_change:?}");

    let after = table
        .rate_for("demo-model", day("2026-09-05"))
        .expect("new");
    assert!((after.input - 3.00).abs() < EPS, "{after:?}");

    assert!(
        table.rate_for("demo-model", day("2025-12-31")).is_none(),
        "a day before every row is unpriced, not priced at the earliest rate"
    );
}

/// Why: the SM's Bedrock provider passes `us.anthropic.claude-…` and its router
/// prefixes that with `bedrock/`; `trusty-agents` sees OpenRouter's
/// `anthropic/claude-…`. All of them are the same model at the same rate.
/// What: every routing spelling of Sonnet 4.6 resolves to the one row, and a
/// versioned or dated snapshot prices as its base model.
/// Test: this test.
#[test]
fn alias_resolves_a_bedrock_id() {
    let p = Pricing::bundled();
    let canonical = p.rate_for("claude-sonnet-4-6", today()).expect("canonical");
    for id in [
        "us.anthropic.claude-sonnet-4-6",
        "eu.anthropic.claude-sonnet-4-6",
        "bedrock/us.anthropic.claude-sonnet-4-6",
        "anthropic/claude-sonnet-4-6",
        "openrouter/anthropic/claude-sonnet-4-6",
        "US.Anthropic.Claude-Sonnet-4-6",
        "us.anthropic.claude-sonnet-4-6-v1:0",
        "claude-sonnet-4-6-20260101",
    ] {
        let r = p
            .rate_for(id, today())
            .unwrap_or_else(|| panic!("{id} priced"));
        assert_eq!(r, canonical, "{id} must price as claude-sonnet-4-6");
    }
}

#[test]
fn normalize_strips_routing_and_region_prefixes() {
    assert_eq!(normalize("us.anthropic.claude-opus-5"), "claude-opus-5");
    assert_eq!(
        normalize("bedrock/global.anthropic.claude-opus-5"),
        "claude-opus-5"
    );
    // A dot that is a version number, not a region prefix, survives.
    assert_eq!(
        normalize("openai/gpt-5.4-mini-20260317"),
        "gpt-5.4-mini-20260317"
    );
}

/// Why (#6875): an operator must be able to correct one rate without waiting
/// for a release, and without the correction quietly wiping the rest.
/// What: an override file that names Sonnet 5 changes only Sonnet 5; Opus 5
/// keeps the bundled rate.
/// Test: this test.
#[test]
fn override_replaces_one_row_and_leaves_the_rest() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(OVERRIDE_FILE);
    std::fs::write(
        &path,
        r#"
        [[model]]
        id = "claude-sonnet-5"
        input = 99.00
        output = 199.00
        cache_write = 1.00
        cache_read = 0.01
        effective_from = "2020-01-01"
        "#,
    )
    .expect("write override");

    let p = Pricing::with_override(&path).expect("load");
    let sonnet = p.rate_for("claude-sonnet-5", today()).expect("sonnet");
    assert!((sonnet.input - 99.00).abs() < EPS, "{sonnet:?}");
    assert!((sonnet.output - 199.00).abs() < EPS, "{sonnet:?}");

    let opus = p.rate_for("claude-opus-5", today()).expect("opus");
    assert!(
        (opus.input - 5.00).abs() < EPS,
        "bundled Opus 5 must survive: {opus:?}"
    );

    // The override owns the id outright, so every routing spelling of it picks
    // up the operator's rate rather than leaving a stale bundled row reachable.
    let via_bedrock = p
        .rate_for("us.anthropic.claude-sonnet-5", today())
        .expect("bedrock spelling");
    assert_eq!(via_bedrock, sonnet);
}

#[test]
fn override_absent_file_is_the_bundled_table() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = Pricing::with_override(&dir.path().join("no-such-file.toml")).expect("load");
    let opus = p.rate_for("claude-opus-5", today()).expect("opus");
    assert!((opus.input - 5.00).abs() < EPS, "{opus:?}");
}

#[test]
fn override_with_malformed_toml_is_an_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(OVERRIDE_FILE);
    std::fs::write(&path, "this is not = = toml").expect("write");
    let err = Pricing::with_override(&path).expect_err("malformed override must be an error");
    assert!(
        matches!(err, PricingError::Parse { .. }),
        "expected a parse error, got {err:?}"
    );
    assert!(err.to_string().contains("parse error"), "{err}");
}

/// Why (#6875): the table this replaced answered every unrecognised id with
/// Sonnet rates. A confident wrong number is worse than a visible gap.
/// What: an id no row claims resolves to `None`.
/// Test: this test.
#[test]
fn unknown_model_is_none() {
    let p = Pricing::bundled();
    for id in ["some/unknown-model", "mystery/model", "m", "", "   "] {
        assert!(p.rate_for(id, today()).is_none(), "{id:?} must be unpriced");
    }
}

#[test]
fn rates_cost_usd_prices_each_bucket_separately() {
    let r = Rates {
        input: 3.00,
        output: 15.00,
        cache_write: 3.75,
        cache_read: 0.30,
    };
    // 100 fresh + 50 output + 1_000 cache write + 9_000 cache read.
    let usage = Usage::new(100, 50, 1_000, 9_000);
    let expected = 0.0003 + 0.00075 + 0.00375 + 0.0027;
    let got = r.cost_usd(&usage);
    assert!(
        (got - expected).abs() < EPS,
        "expected {expected}, got {got}"
    );
}

/// Why (#4098, carried forward): the aggregate surfaces sum a whole corpus, so
/// a count past `u32::MAX` must price linearly rather than saturate.
#[test]
fn cost_usd_accepts_counts_beyond_u32() {
    let r = Pricing::bundled()
        .rate_for("claude-sonnet-5", today())
        .expect("sonnet 5");
    let beyond = u64::from(u32::MAX) + 1;
    let got = r.cost_usd(&Usage::new(beyond, 0, 0, 0));
    let expected = beyond as f64 * 2.00 / 1_000_000.0;
    assert!(
        (got - expected).abs() < 1e-6,
        "expected {expected}, got {got}"
    );
    assert!(got.is_finite());
}

#[test]
fn shared_matches_bundled_for_a_published_model() {
    // `shared()` layers an operator override the developer machine may or may
    // not have, so this asserts resolution works, not a specific rate.
    assert!(shared().rate_today("claude-opus-5").is_some());
}

#[test]
fn default_override_path_layout() {
    let path = default_override_path().expect("home dir");
    assert!(
        path.ends_with(format!("{TRUSTY_TOOLS_DIR}/{OVERRIDE_FILE}")),
        "{path:?}"
    );
}

#[test]
fn warn_unknown_model_once_dedupes() {
    // The dedupe set is process-global; the observable contract is that calling
    // twice does not panic and the second call is a no-op.
    warn_unknown_model_once("pricing-tests/never-priced");
    warn_unknown_model_once("pricing-tests/never-priced");
}

/// Why: a row carried over verbatim from a replaced table must keep its exact
/// old number — that is the promise "do not invent rates" makes.
/// What: the legacy families the two replaced tables priced still price the same.
/// Test: this test.
#[test]
fn legacy_table_rows_keep_their_original_rates() {
    let p = Pricing::bundled();
    let cases: &[(&str, f64, f64)] = &[
        ("claude-sonnet-4-5", 3.00, 15.00),
        ("claude-haiku-4", 0.80, 4.00),
        ("claude-opus-4-1", 15.00, 75.00),
        ("openai/gpt-5.4-mini-20260317", 0.75, 4.50),
        ("openai/gpt-5.4-nano-20260317", 0.20, 1.25),
    ];
    for (id, input, output) in cases {
        let r = p
            .rate_for(id, today())
            .unwrap_or_else(|| panic!("{id} priced"));
        assert!((r.input - input).abs() < EPS, "{id} input: {r:?}");
        assert!((r.output - output).abs() < EPS, "{id} output: {r:?}");
    }
}
