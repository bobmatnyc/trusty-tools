//! Model-eval harness: recall, false positives, hallucinations and cost per
//! reviewer model over 40 labelled diffs (Q86 ruling, AQ-86).
//!
//! Why: the reviewer default (Sonnet 5.5) should be chosen on measured recall,
//! false positives and cost, not on a hunch. The 16-case citation corpus holds
//! about 2 real defects, so it cannot measure recall. This harness adds a
//! labelled dataset: 24 seeded defects, 8 clean diffs and 8 file-scoped diffs
//! of commits that introduced a later-fixed bug (R1-R8).
//!
//! What: every diff goes through the real `run_review` (citation gate,
//! verifier, verdict) and is scored by `support/eval.rs`:
//! - caught: a survivor whose file matches the label, whose cited line is in
//!   a labelled span, and whose body names an anchor token;
//! - false positive: any survivor on a clean diff; extra: a survivor on a
//!   labelled diff that does not catch it;
//! - hallucination: a survivor the shared oracle (`support/oracle.rs`, also
//!   used by the corpus test) cannot resolve at the head.
//!
//! Two legs share that driver:
//! - offline (CI): recorded reviewer outputs in `fixtures/model_eval/recorded/`
//!   replayed by `FakeLlm`, asserting exact scores; no network;
//! - live: `live_model_comparison`, `#[ignore]` AND inert unless
//!   `TRUSTY_EVAL_LIVE=1`; Bedrock reviewers x passes, a Haiku 4.5 verifier, a
//!   cost cap (`TRUSTY_EVAL_MAX_USD`, default 22.00) and a JSON report. See
//!   `fixtures/model_eval/README.md`.
//!
//! Test: `perfect_recording_catches_every_defect`,
//! `bad_recording_scores_hallucinations`, `a_missed_defect_scores_a_miss`,
//! `invented_survivor_on_clean_diff_is_a_false_positive`,
//! `every_label_span_exists_in_its_fixture_diff`, `live_leg_is_inert_without_opt_in`.

#[path = "support/eval.rs"]
mod eval;
#[path = "fakes/mod.rs"]
mod fakes;
#[path = "support/live.rs"]
mod live;
#[path = "support/metered.rs"]
mod metered;
#[path = "support/oracle.rs"]
mod oracle;

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use trusty_review::llm::{LlmError, LlmProvider, LlmRequest, LlmResponse};

use eval::{Entry, Label, Row, Tier, Totals, caught, eval_config, load_dataset, review_diff};
use fakes::{FakeLlm, FakeVerifier};
use live::{LiveOutcome, live_settings, run_live};
use metered::{Budget, Metered};

// ── Recordings ──────────────────────────────────────────────────────────

/// One recorded reviewer reply for one diff.
#[derive(Deserialize)]
struct Output {
    /// The reviewer JSON: `prose`, `verdict`, `grade`, `findings`.
    reviewer: serde_json::Value,
    /// Finding titles the verifier refutes; every other finding is confirmed.
    #[serde(default)]
    refuted: Vec<String>,
}

/// A recorded "model": its id (priced as that model) and one reply per diff.
/// A diff with no entry gets an empty APPROVE.
#[derive(Deserialize)]
struct Recording {
    model: String,
    outputs: BTreeMap<String, Output>,
}

fn load_recording(name: &str) -> Recording {
    let path = eval::fixture_dir()
        .join("recorded")
        .join(format!("{name}.json"));
    let text = std::fs::read_to_string(&path).expect("recording is readable");
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name}.json does not parse: {e}"))
}

/// The reply text a model would send: prose, then the JSON block.
fn reply_text(reviewer: &serde_json::Value) -> String {
    let prose = reviewer["prose"].as_str().unwrap_or_default();
    format!("{prose}\n\n```json\n{reviewer}\n```")
}

const EMPTY_APPROVE: &str =
    r#"{"prose":"Looks good.","verdict":"APPROVE","summary":"LGTM","findings":[]}"#;

/// Replay `rec` over `entries` through the real pipeline.
async fn replay(rec: &Recording, entries: &[Entry]) -> Vec<Row> {
    let config = eval_config(trusty_review::llm::models::DEFAULT_VERIFIER_MODEL);
    let budget = Budget::unbounded();
    let empty: serde_json::Value = serde_json::from_str(EMPTY_APPROVE).expect("valid JSON");
    let mut rows = Vec::with_capacity(entries.len());
    for entry in entries {
        let out = rec.outputs.get(&entry.id);
        let reviewer = out.map_or(&empty, |o| &o.reviewer);
        let refuted = out.map(|o| o.refuted.clone()).unwrap_or_default();
        let llm = Arc::new(FakeLlm {
            response: reply_text(reviewer),
        });
        let verifier = Arc::new(FakeVerifier { refuted });
        rows.push(review_diff(&config, entry, &rec.model, 1, llm, verifier, &budget).await);
    }
    rows
}

fn entries(ids: &[&str]) -> Vec<Entry> {
    let all = load_dataset();
    ids.iter()
        .map(|id| {
            all.iter()
                .find(|e| e.id == *id)
                .unwrap_or_else(|| panic!("no dataset entry `{id}`"))
                .clone()
        })
        .collect()
}

fn row<'a>(rows: &'a [Row], id: &str) -> &'a Row {
    rows.iter()
        .find(|r| r.diff == id)
        .unwrap_or_else(|| panic!("no row for `{id}`"))
}

fn dump(rows: &[Row]) -> String {
    rows.iter()
        .map(|r| {
            format!(
                "{} caught={:?} survivors={} fp={} extras={} halluc={} withheld={:?} verdict={} err={:?}",
                r.diff,
                r.score.caught,
                r.score.survivors,
                r.score.false_positives,
                r.score.extras,
                r.score.hallucinations,
                r.score.withheld_by_reason,
                r.score.verdict,
                r.score.error
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// ── Dataset ─────────────────────────────────────────────────────────────

/// The dataset is the ruling's 40: 24 seeded, 8 clean, 8 real; ids unique;
/// every labelled tier has a label and no clean diff has one.
#[test]
fn dataset_matches_the_ruling() {
    let all = load_dataset();
    let count = |t: Tier| all.iter().filter(|e| e.tier == t).count();
    assert_eq!(
        (count(Tier::Seeded), count(Tier::Clean), count(Tier::Real)),
        (24, 8, 8)
    );
    let mut ids: Vec<&str> = all.iter().map(|e| e.id.as_str()).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 40, "dataset ids are unique");
    for e in &all {
        assert!(!e.about.is_empty(), "{}: says what it plants", e.id);
        assert_eq!(
            e.label.is_some(),
            e.tier != Tier::Clean,
            "{}: a label iff not clean",
            e.id
        );
    }
}

/// Every label names a file in its diff, and every line of every span is a
/// new-side line of that file (a removal's span may sit at its deletion).
#[test]
fn every_label_span_exists_in_its_fixture_diff() {
    for e in load_dataset() {
        let Some(label) = &e.label else { continue };
        let (head, base) = oracle::diff_lines(&e.diff());
        let lines = oracle::file_lines(&head, &label.file)
            .unwrap_or_else(|| panic!("{}: `{}` is not in the diff", e.id, label.file));
        let removed = oracle::file_lines(&base, &label.file);
        assert!(
            !label.spans.is_empty() && !label.anchors.is_empty(),
            "{}",
            e.id
        );
        for [a, b] in &label.spans {
            assert!(a <= b, "{}: span {a}-{b} is reversed", e.id);
            for l in *a..=*b {
                let at_removal = label.removal && removed.is_some_and(|r| r.contains_key(&l));
                assert!(
                    lines.contains_key(&l) || at_removal,
                    "{}: line {l} of span {a}-{b} is not in the diff",
                    e.id
                );
            }
        }
    }
}

// ── Scoring ─────────────────────────────────────────────────────────────

/// "Caught" needs all three: file, line inside a span, anchor in the body.
#[test]
fn catch_requires_file_line_and_anchor() {
    let label = Label {
        file: "src/verdict.rs".into(),
        spans: vec![[14, 20]],
        anchors: vec!["max".into(), "min".into()],
        removal: false,
        kind: "logic".into(),
    };
    let body = "`severities.iter().max()` picks the most severe, not the floor";
    let cases = [
        ("src/verdict.rs", Some(15), body, true),
        ("crates/x/src/verdict.rs", Some(14), body, true),
        ("src/other.rs", Some(15), body, false),
        ("src/verdict.rs", Some(21), body, false),
        ("src/verdict.rs", None, body, false),
        (
            "src/verdict.rs",
            Some(15),
            "`severities.iter()` is wrong",
            false,
        ),
    ];
    for (file, line, desc, want) in cases {
        assert_eq!(
            caught(&label, file, line, desc),
            want,
            "{file}:{line:?} {desc}"
        );
    }
}

// ── Offline leg: recorded outputs, exact scores ─────────────────────────

/// A recording that finds every defect, grounded, scores recall 32/32 with no
/// false positive, extra or hallucination.
#[tokio::test]
async fn perfect_recording_catches_every_defect() {
    let rec = load_recording("perfect");
    let rows = replay(&rec, &load_dataset()).await;
    let t = Totals::of(&rows);
    eprintln!("perfect:\n{}", dump(&rows));
    eprintln!(
        "{}",
        eval::markdown_table(&[(rec.model.clone(), 1, t.clone())])
    );
    assert_eq!(
        (
            t.diffs,
            t.defects,
            t.caught,
            t.false_positives,
            t.extras,
            t.hallucinations,
            t.failed_reviews
        ),
        (40, 32, 32, 0, 0, 0, 0),
        "{}",
        dump(&rows)
    );
    assert!(t.reviewer.cost_usd > 0.0 && t.verifier.cost_usd > 0.0);
    assert_eq!(t.reviewer.calls as usize, 40, "one reviewer call per diff");
}

/// A deliberately bad recording: a finding that quotes real code but names a
/// function the file does not have passes the gate (a bare identifier anchors
/// nothing, #9188 E) and fails the oracle, so hallucinations go above 0.
#[tokio::test]
async fn bad_recording_scores_hallucinations() {
    let rec = load_recording("bad");
    let rows = replay(&rec, &load_dataset()).await;
    let t = Totals::of(&rows);
    eprintln!("bad:\n{}", dump(&rows));
    assert!(t.hallucinations > 0, "{}", dump(&rows));
    assert_eq!(
        (t.caught, t.false_positives, t.extras, t.hallucinations),
        BAD_TOTALS,
        "{}",
        dump(&rows)
    );
    let h = row(&rows, "L3");
    assert_eq!((h.score.hallucinations, h.score.caught), (1, Some(true)));
    assert!(h.score.unresolved[0].body.contains("oldest_pending_seq"));
}

/// Expected (caught, false positives, extras, hallucinations) for `bad.json`.
const BAD_TOTALS: (usize, usize, usize, usize) = (2, 1, 2, 1);

/// One recording misses a seeded defect and one finds it. A finding on the
/// right file outside the span, or without an anchor, is a miss and an extra.
#[tokio::test]
async fn a_missed_defect_scores_a_miss() {
    let ids = ["L1", "E1", "C1", "B1"];
    let found = replay(&load_recording("perfect"), &entries(&ids)).await;
    let missed = replay(&load_recording("bad"), &entries(&ids)).await;
    eprintln!("found:\n{}\nmissed:\n{}", dump(&found), dump(&missed));
    for id in ids {
        assert_eq!(row(&found, id).score.caught, Some(true), "{id} found");
    }
    // L1: no finding. E1: right line, no anchor. C1: grounded, outside the span.
    for (id, extras) in [("L1", 0), ("E1", 1), ("C1", 1)] {
        let r = row(&missed, id);
        assert_eq!(
            (r.score.caught, r.score.extras),
            (Some(false), extras),
            "{id}"
        );
        assert_eq!(r.score.hallucinations, 0, "{id} is grounded");
    }
    // B1: the right finding, refuted by the verifier, does not survive.
    let b1 = row(&missed, "B1");
    assert_eq!((b1.score.caught, b1.score.survivors), (Some(false), 0));
}

/// An invented but grounded survivor on a clean diff is a false positive; an
/// empty review of a clean diff is not.
#[tokio::test]
async fn invented_survivor_on_clean_diff_is_a_false_positive() {
    let ids = ["K2", "K5"];
    let bad = replay(&load_recording("bad"), &entries(&ids)).await;
    let good = replay(&load_recording("perfect"), &entries(&ids)).await;
    eprintln!("bad:\n{}\ngood:\n{}", dump(&bad), dump(&good));
    let k2 = row(&bad, "K2");
    assert_eq!((k2.score.false_positives, k2.score.hallucinations), (1, 0));
    assert_eq!(row(&bad, "K5").score.false_positives, 0);
    assert_eq!(Totals::of(&good).false_positives, 0);
}

// ── Metering and the cost cap ───────────────────────────────────────────

/// A provider that reports fixed token counts for a fixed model.
struct Priced {
    model: &'static str,
    input: u32,
    output: u32,
}

#[async_trait]
impl LlmProvider for Priced {
    fn name(&self) -> &str {
        "priced"
    }
    async fn complete(&self, _: LlmRequest) -> Result<LlmResponse, LlmError> {
        Ok(LlmResponse {
            text: String::new(),
            model: self.model.into(),
            input_tokens: self.input,
            output_tokens: self.output,
            latency_ms: 7,
            cost_usd: 0.0,
            finish_reason: None,
        })
    }
}

fn request() -> LlmRequest {
    LlmRequest {
        model: String::new(),
        system: String::new(),
        messages: vec![],
        temperature: 0.0,
        max_tokens: 1,
        response_schema: None,
    }
}

const SONNET_55: &str = "us.anthropic.claude-sonnet-5-5";
const HAIKU_45: &str = "us.anthropic.claude-haiku-4-5-20251001-v1:0";

/// Each call costs $0.011 (Sonnet 5.5, 5k in at $2.20/M = $0.011); a $0.02 cap
/// lets two calls through and refuses the third before it reaches the provider.
#[tokio::test]
async fn cost_cap_refuses_the_next_call_once_reached() {
    let budget = Budget::new(0.02);
    let inner = Arc::new(Priced {
        model: SONNET_55,
        input: 5_000,
        output: 0,
    });
    let m = Metered::new(inner, Arc::clone(&budget));
    assert!(m.complete(request()).await.is_ok());
    assert!(!budget.exhausted(), "0.011 < 0.02");
    assert!(m.complete(request()).await.is_ok());
    assert!(budget.exhausted(), "0.022 >= 0.02");
    let err = m.complete(request()).await.expect_err("refused at the cap");
    assert!(err.to_string().contains("cost cap"), "{err}");
    let u = m.usage();
    assert_eq!((u.calls, u.refused), (2, 1));
    assert!((u.cost_usd - 0.022).abs() < 1e-9, "{}", u.cost_usd);
}

/// Reviewer and verifier charge one budget: $0.0132 + $0.0011 crosses a
/// $0.014 cap that neither reaches alone, and the next call is refused.
#[tokio::test]
async fn meter_sums_reviewer_and_verifier_cost() {
    let budget = Budget::new(0.014);
    let reviewer = Metered::new(
        Arc::new(Priced {
            model: SONNET_55,
            input: 1_000,
            output: 1_000,
        }),
        Arc::clone(&budget),
    );
    let verifier = Metered::new(
        Arc::new(Priced {
            model: HAIKU_45,
            input: 1_000,
            output: 0,
        }),
        Arc::clone(&budget),
    );
    reviewer.complete(request()).await.expect("reviewer call");
    verifier.complete(request()).await.expect("verifier call");
    // 0.0022 + 0.011 reviewer, 0.0011 verifier.
    let want = 0.0132 + 0.0011;
    assert!((budget.spent() - want).abs() < 1e-9, "{}", budget.spent());
    assert!(budget.exhausted());
    assert!(verifier.complete(request()).await.is_err());
    assert_eq!(verifier.usage().refused, 1);
}

// ── Live leg ────────────────────────────────────────────────────────────

fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();
    move |k| map.get(k).cloned()
}

fn panicking_factory(model: &str) -> Result<Arc<dyn LlmProvider>, String> {
    panic!("a provider for `{model}` was built without TRUSTY_EVAL_LIVE=1")
}

/// Without `TRUSTY_EVAL_LIVE=1` the live leg returns before building any
/// provider: the factory, the only place a network provider is made, panics
/// if called.
#[tokio::test]
async fn live_leg_is_inert_without_opt_in() {
    for value in [None, Some("0"), Some("true"), Some("")] {
        let pairs: Vec<(&str, &str)> = value.map(|v| ("TRUSTY_EVAL_LIVE", v)).into_iter().collect();
        let env = env_of(&pairs);
        let outcome = run_live(&env, &panicking_factory).await.expect("no error");
        assert!(matches!(outcome, LiveOutcome::Skipped), "{value:?}");
    }
}

/// The overrides parse; defaults are the ruling's 4 models, 3 passes, $22.
#[test]
fn live_settings_read_overrides() {
    let env = env_of(&[("TRUSTY_EVAL_LIVE", "1")]);
    let s = live_settings(&env).expect("parses").expect("enabled");
    assert_eq!(
        (s.models.len(), s.passes, s.max_usd, s.verifier.as_str()),
        (4, 3, 22.0, HAIKU_45)
    );
    let env = env_of(&[
        ("TRUSTY_EVAL_LIVE", "1"),
        (
            "TRUSTY_EVAL_MODELS",
            "bedrock/us.anthropic.claude-opus-5-5, us.anthropic.claude-sonnet-4-6",
        ),
        ("TRUSTY_EVAL_PASSES", "1"),
        ("TRUSTY_EVAL_MAX_USD", "2.5"),
        ("TRUSTY_EVAL_ONLY", "L1,R3"),
    ]);
    let s = live_settings(&env).expect("parses").expect("enabled");
    assert_eq!(s.models.len(), 2);
    assert_eq!((s.passes, s.max_usd, s.only.len()), (1, 2.5, 2));
    let bad = env_of(&[("TRUSTY_EVAL_LIVE", "1"), ("TRUSTY_EVAL_PASSES", "x")]);
    assert!(live_settings(&bad).is_err());
}

/// A model with no Bedrock price would meter as $0 and slip past the cap, so
/// the live leg refuses to start; the factory is never called.
#[tokio::test]
async fn live_leg_refuses_an_unpriced_model() {
    let env = env_of(&[
        ("TRUSTY_EVAL_LIVE", "1"),
        ("TRUSTY_EVAL_MODELS", "bedrock/us.anthropic.claude-typo-9"),
    ]);
    let err = run_live(&env, &panicking_factory)
        .await
        .expect_err("refused");
    assert!(err.contains("no Bedrock price"), "{err}");
}

/// The live driver with fake providers: it writes a JSON report with the
/// config, git SHA and per-row scores, and stops at the cap.
#[tokio::test]
async fn live_leg_writes_a_report_and_stops_at_the_cap() {
    let out = tempfile::tempdir().expect("tempdir");
    let out_dir = out.path().to_string_lossy().into_owned();
    let env = env_of(&[
        ("TRUSTY_EVAL_LIVE", "1"),
        ("TRUSTY_EVAL_MODELS", SONNET_55),
        ("TRUSTY_EVAL_PASSES", "3"),
        ("TRUSTY_EVAL_MAX_USD", "0.0001"),
        ("TRUSTY_EVAL_CONCURRENCY", "1"),
        ("TRUSTY_EVAL_ONLY", "L1,K1"),
        ("TRUSTY_EVAL_OUT_DIR", &out_dir),
    ]);
    let factory = |_: &str| -> Result<Arc<dyn LlmProvider>, String> {
        Ok(Arc::new(FakeLlm {
            response: reply_text(&serde_json::from_str(EMPTY_APPROVE).expect("valid")),
        }))
    };
    let LiveOutcome::Ran {
        report,
        stopped,
        rows,
        spent_usd,
    } = run_live(&env, &factory).await.expect("runs")
    else {
        panic!("enabled run skipped");
    };
    assert_eq!(rows, 2, "one pass of 2 diffs, then the cap stops it");
    assert!(stopped.is_some_and(|s| s.contains("cost cap")));
    assert!(spent_usd >= 0.0001);
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&report).expect("report")).expect("JSON");
    assert_eq!(json["config"]["passes"], 3);
    assert!(json.get("git_sha").is_some());
    let row = &json["rows"][0];
    for key in [
        "model",
        "pass",
        "diff",
        "caught",
        "false_positives",
        "hallucinations",
        "withheld_by_reason",
        "reviewer",
        "verifier",
        "wall_ms",
    ] {
        assert!(row.get(key).is_some(), "row lacks `{key}`: {row}");
    }
}

/// The live comparison. Inert unless `TRUSTY_EVAL_LIVE=1`; see
/// `fixtures/model_eval/README.md` for the run command and cost cap.
#[tokio::test]
#[ignore = "live Bedrock; set TRUSTY_EVAL_LIVE=1 and run with --include-ignored"]
async fn live_model_comparison() {
    let env = |k: &str| std::env::var(k).ok();
    let factory = |model: &str| -> Result<Arc<dyn LlmProvider>, String> {
        let bare = trusty_review::llm::strip_provider_prefix(model);
        trusty_review::llm::BedrockProvider::new(bare)
            .map(|p| Arc::new(p) as Arc<dyn LlmProvider>)
            .map_err(|e| e.to_string())
    };
    match run_live(&env, &factory).await {
        Ok(LiveOutcome::Skipped) => {
            eprintln!("live_model_comparison: TRUSTY_EVAL_LIVE!=1, skipped")
        }
        Ok(LiveOutcome::Ran {
            report, spent_usd, ..
        }) => {
            eprintln!(
                "live_model_comparison: ${spent_usd:.4}, {}",
                report.display()
            );
        }
        Err(e) => panic!("live_model_comparison: {e}"),
    }
}
