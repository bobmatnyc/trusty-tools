//! The offline zero-hallucination corpus runner (#9188).
//!
//! Why: #9188 sets the threshold at 0 hallucinated findings per review. One
//! regression test per leak proves each leak closed; this proves the whole
//! pipeline, end to end, over a fixed corpus that does not depend on a model
//! or the network, so it can later be rerun with a model's recorded output.
//! What: for each case in `tests/fixtures/citation_corpus/`, drives
//! `run_review` with a stub reviewer that returns the case's `reviewer` JSON
//! and a stub verifier that answers the case's `verifier` judgment, then
//! counts hallucinations: survivors labelled false or that `oracle::resolves`
//! cannot resolve at the head, forbidden prose in the body, and a review with
//! no survivor whose verdict blocks or whose grade a withheld finding shaped.
//! It also checks each case's survivor count and verdict (`expect_verdict`;
//! AQ-7t, Bob 2026-10-05: an all-withheld APPROVE / APPROVE* keeps it).
//! Test: `hallucination_count_is_zero`.

use std::path::Path;

use serde::Deserialize;

use super::*;

// The resolver is shared with `tests/model_eval.rs`, so it lives under `tests/`.
#[path = "../../tests/support/oracle.rs"]
mod oracle;

/// One corpus case; see `tests/fixtures/citation_corpus/README.md`.
#[derive(Deserialize)]
struct Case {
    leak: String,
    #[serde(default)]
    diff: Vec<String>,
    #[serde(default)]
    diff_file: Option<String>,
    #[serde(default)]
    pr_description: Option<String>,
    reviewer: serde_json::Value,
    verifier: String,
    hallucinated: Vec<String>,
    /// Titles of findings that are about a removal, by ground truth; their
    /// quotes resolve against the removed (base) lines too (#9188 F).
    #[serde(default)]
    removals: Vec<String>,
    forbidden_in_body: Vec<String>,
    expect_survivors: usize,
    /// The verdict the review must end with, as `run --json` prints it.
    expect_verdict: String,
}

fn corpus_dir() -> &'static Path {
    Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/citation_corpus"
    ))
}

fn load_cases() -> Vec<(String, Case)> {
    let mut paths: Vec<_> = std::fs::read_dir(corpus_dir())
        .expect("corpus dir is readable")
        .map(|e| e.expect("corpus entry").path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|p| {
            let text = std::fs::read_to_string(&p).expect("case is readable");
            let case: Case = serde_json::from_str(&text)
                .unwrap_or_else(|e| panic!("{} does not parse: {e}", p.display()));
            let name = p
                .file_stem()
                .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
            (name, case)
        })
        .collect()
}

fn case_diff(case: &Case) -> String {
    match &case.diff_file {
        Some(file) => {
            std::fs::read_to_string(corpus_dir().join(file)).expect("diff file is readable")
        }
        None => format!("{}\n", case.diff.join("\n")),
    }
}

/// The stub verifier's judgment; the fake needs a `'static` string.
fn judgment(word: &str) -> &'static str {
    match word {
        "CONFIRMED" => "CONFIRMED",
        "REFUTED" => "REFUTED",
        "UNVERIFIABLE" => "UNVERIFIABLE",
        other => panic!("unknown verifier judgment `{other}`"),
    }
}

async fn run_case(case: &Case, diff: &str) -> ReviewResult {
    let (source, _tmp) = local_diff_source(diff);
    let prose = case.reviewer["prose"].as_str().unwrap_or_default();
    let llm = FakeLlm {
        response: format!("{prose}\n\n```json\n{}\n```", case.reviewer),
        error: None,
        output_tokens: None,
    };
    let input = ReviewInput {
        diff_source: source,
        reviewer_model: "stub/corpus-reviewer".to_string(),
        write_log: false,
        print_result: false,
        trigger: TriggerDecision::None,
        run_mode: RunMode::Cli,
        allow_posting: false,
        caller_context: CallerContext {
            pr_description: case.pr_description.clone(),
            ..CallerContext::default()
        },
        surface: InvocationSurface::default(),
    };
    let verifier: Arc<dyn LlmProvider> = Arc::new(FakeVerifier {
        judgment: judgment(&case.verifier),
    });
    run_review(
        &default_config(),
        input,
        ready_deps(Arc::new(llm), Some(verifier)),
    )
    .await
}

/// Hallucinations in one reviewed case.
fn hallucinations(case: &Case, diff: &str, result: &ReviewResult) -> usize {
    let lines = oracle::diff_lines(diff);
    let survivors = result
        .findings
        .iter()
        .filter(|f| {
            let removal = case.removals.contains(&f.kind);
            case.hallucinated.contains(&f.kind)
                || !oracle::resolves(&f.file, f.line, &f.description, &lines, removal)
        })
        .count();
    let prose = case
        .forbidden_in_body
        .iter()
        .filter(|p| result.review_body.contains(p.as_str()))
        .count();
    // AQ-7t (Bob 2026-10-05): with no survivor, a withheld finding may neither
    // block nor shape the grade.
    let withheld_shapes_verdict = oracle::withheld_shapes_verdict(
        result.findings.len(),
        result.withheld_findings.len(),
        &result.verdict.to_string(),
        result.grade.as_deref(),
    );
    survivors + prose + usize::from(withheld_shapes_verdict)
}

/// #9188: every survivor over the corpus resolves at the head; no prose names
/// an unbacked defect; no withheld finding blocks or grades a review. Each case
/// ends with its expected survivors and verdict (AQ-7t).
#[tokio::test]
async fn hallucination_count_is_zero() {
    let cases = load_cases();
    assert!(
        cases.len() >= 9,
        "the corpus names at least the issue's 9 cases"
    );
    let (mut total, mut report) = (0usize, Vec::new());
    for (name, case) in &cases {
        let diff = case_diff(case);
        let result = run_case(case, &diff).await;
        let count = hallucinations(case, &diff, &result);
        total += count;
        let survivors = result.findings.len();
        let verdict = result.verdict.to_string();
        let mut flag = String::new();
        if survivors != case.expect_survivors {
            flag.push_str(" SURVIVORS-MISMATCH");
        }
        if verdict != case.expect_verdict {
            flag.push_str(" VERDICT-MISMATCH");
        }
        report.push(format!(
            "{name} [leak {}]: hallucinations={count} survivors={survivors}/{} verdict={verdict}/{}{flag}",
            case.leak, case.expect_survivors, case.expect_verdict
        ));
    }
    eprintln!("hallucination corpus:\n{}", report.join("\n"));
    let mismatched = report.iter().any(|l| l.contains("-MISMATCH"));
    assert!(
        total == 0 && !mismatched,
        "hallucination_count={total}\n{}",
        report.join("\n")
    );
}
