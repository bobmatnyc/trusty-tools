//! Dataset, scoring and the per-diff review driver of the model-eval harness.
//!
//! Why: the offline leg and the live leg must score the same way, so both go
//! through one driver and one scorer; only the providers differ.
//! What: [`load_dataset`] reads `tests/fixtures/model_eval/dataset.json`;
//! [`review_diff`] runs one diff through the real `run_review` (citation gate,
//! verifier, verdict) with metered providers; [`score`] grades the result
//! against the diff's label with the shared oracle; [`Totals`] and
//! [`markdown_table`] summarise rows per model.
//! Test: `tests/model_eval.rs`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use trusty_review::config::{InvocationSurface, ReviewConfig};
use trusty_review::integrations::github::RunMode;
use trusty_review::llm::LlmProvider;
use trusty_review::models::ReviewResult;
use trusty_review::pipeline::verify_posted::UNJUDGED_REASON;
use trusty_review::pipeline::withheld_contract::reason_class;
use trusty_review::pipeline::{
    CallerContext, DiffSource, ReviewDeps, ReviewInput, TriggerDecision, run_review,
};

use crate::fakes::{FakeSearch, ReadyAnalyze};
use crate::metered::{Budget, Metered, Usage};
use crate::oracle;

/// The fixture directory.
pub fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/model_eval")
}

/// One labelled diff's tier (Q86 ruling: 24 seeded, 8 clean, 8 real).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    /// A hand-authored diff with one planted defect.
    Seeded,
    /// A hand-authored diff with no defect.
    Clean,
    /// A file-scoped diff of the commit that introduced a later-fixed bug.
    Real,
}

/// Where a defect is: file, new-side line spans, and the tokens a finding
/// about it names.
#[derive(Debug, Clone, Deserialize)]
pub struct Label {
    /// The file that holds the defect, as the diff's `+++ b/` path.
    pub file: String,
    /// Inclusive new-side line spans; a removal sits at its deletion's position.
    pub spans: Vec<[u32; 2]>,
    /// A caught finding's body names at least one (case-insensitive).
    pub anchors: Vec<String>,
    /// The defect is a removal: the oracle also reads the removed lines.
    #[serde(default)]
    pub removal: bool,
    /// Defect class, for the report.
    pub kind: String,
}

/// One dataset entry.
#[derive(Debug, Clone, Deserialize)]
pub struct Entry {
    /// Stable id, e.g. `L1`, `K3`, `R5`.
    pub id: String,
    /// Seeded, clean or real.
    pub tier: Tier,
    /// The diff, relative to the fixture directory.
    pub diff_file: String,
    /// The defect; absent for a clean diff.
    #[serde(default)]
    pub label: Option<Label>,
    /// What the diff plants, or which PR it comes from.
    pub about: String,
}

impl Entry {
    /// The diff text.
    pub fn diff(&self) -> String {
        let path = fixture_dir().join(&self.diff_file);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{} is unreadable: {e}", path.display()))
    }
}

/// Every dataset entry, in file order.
pub fn load_dataset() -> Vec<Entry> {
    let path = fixture_dir().join("dataset.json");
    let text = std::fs::read_to_string(&path).expect("dataset.json is readable");
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("dataset.json does not parse: {e}"))
}

/// Whether a finding at `file`:`line` with body `description` catches `label`:
/// the file matches, the line is inside a span, and the body names an anchor.
pub fn caught(label: &Label, file: &str, line: Option<u32>, description: &str) -> bool {
    let same_file = file == label.file
        || label.file.ends_with(&format!("/{file}"))
        || file.ends_with(&format!("/{}", label.file));
    let in_span = line.is_some_and(|l| label.spans.iter().any(|[a, b]| (*a..=*b).contains(&l)));
    let body = description.to_lowercase();
    let named = label
        .anchors
        .iter()
        .any(|a| body.contains(&a.to_lowercase()));
    same_file && in_span && named
}

/// A survivor the oracle cannot resolve, kept for human adjudication.
#[derive(Debug, Clone, Serialize)]
pub struct Unresolved {
    /// The finding's title.
    pub title: String,
    /// Cited file.
    pub file: String,
    /// Cited line.
    pub line: Option<u32>,
    /// The finding's body.
    pub body: String,
}

/// How one reviewed diff scored.
#[derive(Debug, Clone, Default, Serialize)]
pub struct DiffScore {
    /// `Some(true)` caught, `Some(false)` missed, `None` for a clean diff.
    pub caught: Option<bool>,
    /// Findings that survived the gate and the verifier.
    pub survivors: usize,
    /// Survivors on a clean diff.
    pub false_positives: usize,
    /// Survivors on a labelled diff that do not catch its label.
    pub extras: usize,
    /// Survivors the oracle cannot resolve at the head, plus one when a
    /// withheld finding blocked or graded a review with no survivor.
    pub hallucinations: usize,
    /// The unresolved survivors themselves.
    pub unresolved: Vec<Unresolved>,
    /// Withheld findings, by reason.
    pub withheld_by_reason: BTreeMap<String, usize>,
    /// The verdict, as `run --json` prints it.
    pub verdict: String,
    /// `ReviewResult::error`: a failure, or a note that findings were withheld.
    pub error: Option<String>,
}

/// Grade one review of `entry` against its label.
pub fn score(entry: &Entry, diff: &str, result: &ReviewResult) -> DiffScore {
    let lines = oracle::diff_lines(diff);
    let removal = entry.label.as_ref().is_some_and(|l| l.removal);
    let unresolved: Vec<Unresolved> = result
        .findings
        .iter()
        .filter(|f| !oracle::resolves(&f.file, f.line, &f.description, &lines, removal))
        .map(|f| Unresolved {
            title: f.kind.clone(),
            file: f.file.clone(),
            line: f.line,
            body: f.description.clone(),
        })
        .collect();
    let verdict = result.verdict.to_string();
    let status = result.verdict_status.map(|s| s.to_string()); // #9310
    let shaped = oracle::withheld_shapes_verdict(
        result.findings.len(),
        result.withheld_findings.len(),
        &verdict,
        status.as_deref(),
        result.grade.as_deref(),
    );
    let hits = |l: &Label| {
        result
            .findings
            .iter()
            .filter(|f| caught(l, &f.file, f.line, &f.description))
            .count()
    };
    let survivors = result.findings.len();
    let (caught_flag, false_positives, extras) = match &entry.label {
        Some(l) => {
            let n = hits(l);
            (Some(n > 0), 0, survivors - n)
        }
        None => (None, survivors, 0),
    };
    DiffScore {
        caught: caught_flag,
        survivors,
        false_positives,
        extras,
        hallucinations: unresolved.len() + usize::from(shaped),
        unresolved,
        withheld_by_reason: result.withheld_by_reason.clone(),
        verdict,
        error: result.error.clone(),
    }
}

/// One reviewed diff: its score, what each role spent, and wall time.
#[derive(Debug, Clone, Serialize)]
pub struct Row {
    /// Reviewer model id.
    pub model: String,
    /// 1-based pass number.
    pub pass: u32,
    /// Dataset entry id.
    pub diff: String,
    /// The entry's tier.
    pub tier: Tier,
    /// The label's defect class; `None` for a clean diff.
    pub kind: Option<String>,
    /// The score.
    #[serde(flatten)]
    pub score: DiffScore,
    /// Reviewer usage.
    pub reviewer: Usage,
    /// Verifier usage.
    pub verifier: Usage,
    /// Wall time of the whole `run_review` call, ms.
    pub wall_ms: u64,
    /// Why the review did not complete; `None` when it did. An incomplete row
    /// is left out of every score (see [`incomplete_reason`]).
    pub incomplete: Option<String>,
    /// Reviewer reply texts, for the raw-capture join; not in the report.
    #[serde(skip)]
    pub replies: Vec<String>,
    /// Raw-capture file names of this row's reviewer replies (#9310); empty
    /// unless a live run sets `TRUSTY_REVIEW_CAPTURE_DIR`.
    pub capture_files: Vec<String>,
}

impl Row {
    /// Whether the cost cap refused one of this row's calls.
    pub fn cut_by_cap(&self) -> bool {
        self.reviewer.refused > 0 || self.verifier.refused > 0
    }
}

/// Why a review did not complete, or `None`.
///
/// Why: a review cut short is not the model's miss. The runner does not retry
/// a failed reviewer call, and a refused or rejected verifier call withholds
/// findings the model did report.
/// What: incomplete when the reviewer was refused by the cap, never returned,
/// or errored (a throttle included); or when a verifier call was refused by
/// the cap or failed with `LlmError::Validation`; or when a finding was
/// withheld as unjudged (`UNJUDGED_REASON`: the verifier was denied, missing,
/// not ready, failed after retries, or answered unparsably).
pub fn incomplete_reason(
    reviewer: &Usage,
    verifier: &Usage,
    withheld_by_reason: &BTreeMap<String, usize>,
) -> Option<String> {
    let unjudged = reason_class(UNJUDGED_REASON);
    if reviewer.refused > 0 {
        Some("cost cap refused the reviewer".into())
    } else if reviewer.errors > 0 || reviewer.calls == 0 {
        Some("reviewer call failed".into())
    } else if verifier.refused > 0 {
        Some("cost cap refused the verifier".into())
    } else if verifier.validation_errors > 0 {
        Some("verifier call failed validation".into())
    } else if withheld_by_reason.get(unjudged).is_some_and(|n| *n > 0) {
        Some("the verifier could not judge a finding".into())
    } else {
        None
    }
}

/// The config every eval review runs under, built field by field.
///
/// Why: the offline leg must not depend on the operator's
/// `~/.config/trusty-review/config.toml` or environment, and no review may
/// reach Jira, Confluence, GitHub or a daemon.
/// What: Bedrock roles at their built-in defaults with the verifier pinned to
/// `verifier_model`; every external context source disabled; dry run; no
/// GitHub credentials; verification and context gates at their defaults.
pub fn eval_config(verifier_model: &str) -> ReviewConfig {
    use trusty_review::config::{
        ContextConfig, Provider, RoleConfig, RoleModels, VerificationConfig,
    };
    use trusty_review::coverage::CoveragePolicy;
    use trusty_review::integrations::context::{
        ConformanceSourceConfig, ContextSourcesConfig, SourceConfig,
    };
    use trusty_review::llm::models::{DEFAULT_REVIEWER_MODEL, DEFAULT_SUMMARIZER_MODEL};
    let role = |model: &str, temperature: f32, max_tokens: u32| RoleConfig {
        provider: Provider::Bedrock,
        model: model.to_string(),
        temperature,
        max_tokens,
    };
    let off = SourceConfig {
        enabled: Some(false),
        ..SourceConfig::default()
    };
    ReviewConfig {
        dry_run: true,
        enabled_repos: "*".to_string(),
        excluded_repos: String::new(),
        excluded_authors: String::new(),
        log_dir: std::env::temp_dir().join("trusty-review-model-eval"),
        openrouter_api_key: String::new(),
        fireworks_api_key: String::new(),
        // Never dialled: `review_diff` injects `FakeSearch` and `ReadyAnalyze`.
        search_url: "http://127.0.0.1:9".to_string(),
        analyzer_socket: PathBuf::from("/nonexistent/trusty-analyze.sock"),
        search_index: "main".to_string(),
        search_index_explicit: true,
        role_models: RoleModels {
            reviewer: role(DEFAULT_REVIEWER_MODEL, 0.3, 4096),
            verifier: role(verifier_model, 1.0, 128),
            summarizer: role(DEFAULT_SUMMARIZER_MODEL, 0.0, 4096),
        },
        github_app_id: None,
        github_app_private_key: None,
        github_token: String::new(),
        github_installations: Vec::new(),
        bot_username: "trusty-review[bot]".to_string(),
        live_review_requesters: Vec::new(),
        verification: VerificationConfig::default(),
        context: ContextConfig::default(),
        context_sources: ContextSourcesConfig {
            jira: off.clone(),
            confluence: off.clone(),
            github_issues: off.clone(),
            conformance: ConformanceSourceConfig {
                base: off.clone(),
                ..ConformanceSourceConfig::default()
            },
            pr_history: off,
        },
        voice_package: None,
        voice_principles: true,
        review_template: None,
        coverage: CoveragePolicy::default(),
    }
}

/// Review `entry` with `reviewer_model` through the real pipeline, metering
/// `llm` and `verifier` against `budget`.
pub async fn review_diff(
    config: &ReviewConfig,
    entry: &Entry,
    reviewer_model: &str,
    pass: u32,
    llm: Arc<dyn LlmProvider>,
    verifier: Arc<dyn LlmProvider>,
    budget: &Arc<Budget>,
) -> Row {
    use std::io::Write as _;
    let diff = entry.diff();
    let mut tmp = tempfile::NamedTempFile::new().expect("tempfile");
    tmp.write_all(diff.as_bytes()).expect("write diff");
    let reviewer = Metered::new(llm, Arc::clone(budget));
    let checker = Metered::new(verifier, Arc::clone(budget));
    let input = ReviewInput {
        diff_source: DiffSource::LocalFile {
            path: tmp.path().to_path_buf(),
        },
        reviewer_model: reviewer_model.to_string(),
        write_log: false,
        print_result: false,
        trigger: TriggerDecision::None,
        run_mode: RunMode::Cli,
        allow_posting: false,
        caller_context: CallerContext::default(),
        surface: InvocationSurface::default(),
    };
    let deps = ReviewDeps {
        llm: reviewer.clone(),
        verifier: Some(checker.clone()),
        search: Arc::new(FakeSearch),
        analyze: Some(Arc::new(ReadyAnalyze)),
        dedup: None,
    };
    let started = Instant::now();
    let result = run_review(config, input, deps).await;
    let wall_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let replies = reviewer.replies();
    let (reviewer, verifier) = (reviewer.usage(), checker.usage());
    Row {
        model: reviewer_model.to_string(),
        pass,
        diff: entry.id.clone(),
        tier: entry.tier,
        kind: entry.label.as_ref().map(|l| l.kind.clone()),
        incomplete: incomplete_reason(&reviewer, &verifier, &result.withheld_by_reason),
        score: score(entry, &diff, &result),
        reviewer,
        verifier,
        wall_ms,
        replies,
        capture_files: Vec::new(),
    }
}

/// Totals over a set of rows.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Totals {
    /// Complete rows scored.
    pub diffs: usize,
    /// Incomplete rows, left out of every score above and below.
    pub incomplete: usize,
    /// Labelled rows.
    pub defects: usize,
    /// Labelled rows caught.
    pub caught: usize,
    /// Survivors on clean diffs.
    pub false_positives: usize,
    /// Unlabelled survivors on labelled diffs.
    pub extras: usize,
    /// Hallucinations, as [`DiffScore::hallucinations`].
    pub hallucinations: usize,
    /// Withheld findings.
    pub withheld: usize,
    /// Reviewer usage.
    pub reviewer: Usage,
    /// Verifier usage.
    pub verifier: Usage,
    /// Wall time, ms.
    pub wall_ms: u64,
}

impl Totals {
    /// Sum `rows`. Usage and wall time count every row; scores count only
    /// complete rows.
    pub fn of<'a>(rows: impl IntoIterator<Item = &'a Row>) -> Self {
        let mut t = Self::default();
        for r in rows {
            t.reviewer.absorb(&r.reviewer);
            t.verifier.absorb(&r.verifier);
            t.wall_ms += r.wall_ms;
            if r.incomplete.is_some() {
                t.incomplete += 1;
                continue;
            }
            t.diffs += 1;
            if let Some(c) = r.score.caught {
                t.defects += 1;
                t.caught += usize::from(c);
            }
            t.false_positives += r.score.false_positives;
            t.extras += r.score.extras;
            t.hallucinations += r.score.hallucinations;
            t.withheld += r.score.withheld_by_reason.values().sum::<usize>();
        }
        t
    }

    /// Caught over labelled, 0 when there is none.
    pub fn recall(&self) -> f64 {
        if self.defects == 0 {
            0.0
        } else {
            self.caught as f64 / self.defects as f64
        }
    }
}

/// A markdown table with one row per `(model, passes compared, totals)`.
pub fn markdown_table(per_model: &[(String, u32, Totals)]) -> String {
    let mut out = String::from(
        "| model | passes | recall | caught/defects | false positives | extras | hallucinations | withheld | incomplete | reviewer tok in/out | verifier tok in/out | cost USD | mean wall s |\n\
         |---|---|---|---|---|---|---|---|---|---|---|---|---|\n",
    );
    for (model, passes, t) in per_model {
        let rows = t.diffs + t.incomplete;
        let mean_wall = if rows == 0 {
            0.0
        } else {
            t.wall_ms as f64 / rows as f64 / 1000.0
        };
        out.push_str(&format!(
            "| {model} | {passes} | {:.3} | {}/{} | {} | {} | {} | {} | {} | {}/{} | {}/{} | {:.4} | {mean_wall:.1} |\n",
            t.recall(),
            t.caught,
            t.defects,
            t.false_positives,
            t.extras,
            t.hallucinations,
            t.withheld,
            t.incomplete,
            t.reviewer.input_tokens,
            t.reviewer.output_tokens,
            t.verifier.input_tokens,
            t.verifier.output_tokens,
            t.reviewer.cost_usd + t.verifier.cost_usd,
        ));
    }
    out
}
