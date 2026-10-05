//! The offline zero-hallucination corpus runner (#9188).
//!
//! Why: #9188 sets the threshold at 0 hallucinated findings per review. One
//! regression test per leak proves each leak closed; this proves the whole
//! pipeline, end to end, over a fixed corpus that does not depend on a model
//! or the network, so it can later be rerun with a model's recorded output.
//! What: for each case in `tests/fixtures/citation_corpus/`, drives
//! `run_review` with a stub reviewer that returns the case's `reviewer` JSON
//! and a stub verifier that answers the case's `verifier` judgment, then
//! counts hallucinations: survivors labelled false or that [`oracle_resolves`]
//! cannot resolve at the head, forbidden prose in the body, and a review with
//! no survivor that still approves or carries a grade.
//! Test: `hallucination_count_is_zero`.

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

use super::*;
use crate::models::Finding;

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
    forbidden_in_body: Vec<String>,
    expect_survivors: usize,
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

fn norm(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// New-side text of every file in `diff`, by line number. Written apart from
/// the gate so the corpus does not grade the gate with its own code.
fn head_lines(diff: &str) -> HashMap<String, HashMap<u32, String>> {
    let mut files: HashMap<String, HashMap<u32, String>> = HashMap::new();
    let (mut file, mut next) = (String::new(), 0u32);
    for line in diff.lines() {
        if let Some(path) = line.strip_prefix("+++ b/") {
            file = path.trim().to_string();
        } else if let Some(rest) = line.strip_prefix("@@ ") {
            let new = rest.split_whitespace().find_map(|t| t.strip_prefix('+'));
            next = new
                .and_then(|n| n.split(',').next())
                .and_then(|n| n.parse().ok())
                .unwrap_or(0);
        } else if let Some(body) = line.strip_prefix('+').or_else(|| line.strip_prefix(' ')) {
            files
                .entry(file.clone())
                .or_default()
                .insert(next, norm(body));
            next += 1;
        }
    }
    files
}

/// The corpus's own resolver: the finding's file is at the head, its line
/// exists there, every backtick code span it quotes is in that file, and one
/// of them is on the cited line. A finding that quotes no code never resolves.
fn oracle_resolves(f: &Finding, head: &HashMap<String, HashMap<u32, String>>) -> bool {
    let Some(lines) = head
        .iter()
        .find(|(k, _)| **k == f.file || k.ends_with(&format!("/{}", f.file)))
        .map(|(_, v)| v)
    else {
        return false;
    };
    let Some(on_line) = f.line.and_then(|l| lines.get(&l)) else {
        return false;
    };
    let whole: String = lines.values().cloned().collect::<Vec<_>>().join(" ");
    let spans: Vec<String> = f
        .description
        .split('`')
        .skip(1)
        .step_by(2)
        .map(norm)
        .filter(|s| s.len() >= 3 && !s.contains(".rs:"))
        .collect();
    !spans.is_empty()
        && spans.iter().all(|s| whole.contains(s.as_str()))
        && spans.iter().any(|s| on_line.contains(s.as_str()))
}

/// Hallucinations in one reviewed case.
fn hallucinations(case: &Case, diff: &str, result: &ReviewResult) -> usize {
    let head = head_lines(diff);
    let survivors = result
        .findings
        .iter()
        .filter(|f| case.hallucinated.contains(&f.kind) || !oracle_resolves(f, &head))
        .count();
    let prose = case
        .forbidden_in_body
        .iter()
        .filter(|p| result.review_body.contains(p.as_str()))
        .count();
    let approves = matches!(
        result.verdict,
        Verdict::Approve | Verdict::ApproveWithReservations
    );
    let empty_verdict = result.findings.is_empty()
        && !result.withheld_findings.is_empty()
        && (approves || result.grade.is_some());
    survivors + prose + usize::from(empty_verdict)
}

/// #9188: every survivor over the corpus resolves at the head; no prose names
/// an unbacked defect; no all-withheld review approves or carries a grade.
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
        let flag = if survivors == case.expect_survivors {
            ""
        } else {
            " SURVIVORS-MISMATCH"
        };
        report.push(format!(
            "{name} [leak {}]: hallucinations={count} survivors={survivors}/{}{flag}",
            case.leak, case.expect_survivors
        ));
    }
    eprintln!("hallucination corpus:\n{}", report.join("\n"));
    let mismatched = report.iter().any(|l| l.ends_with("SURVIVORS-MISMATCH"));
    assert!(
        total == 0 && !mismatched,
        "hallucination_count={total}\n{}",
        report.join("\n")
    );
}
