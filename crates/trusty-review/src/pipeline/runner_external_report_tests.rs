//! The `external_sources` ledger row through the real pipeline (#9194).
//!
//! Why: the owner comment of 2026-10-07 requires a failed external source to
//! show as `unavailable`; the fail-open gather dropped it without a trace,
//! and the reviewer's prompt must not change because of the report.
//! What: injects scripted sources through `ReviewOptions::external_sources`
//! and reads the `external_sources` row and its per-source items.
//! Test: this module.

use std::sync::atomic::{AtomicUsize, Ordering};

use super::context_report::{Run, row};
use super::*;
use crate::integrations::context::{
    ContextSource, GithubIssuesSource, RetrievalMode,
    orchestrator::outcome_tests::{Answer, CountingSearch, Token, scripted},
};
use crate::models::{ContextItemRecord, ContextSourceRecord, SourceState};
use crate::pipeline::optional_context::OptionalContextRequest;

fn report() -> OptionalContextRequest {
    OptionalContextRequest::default().with_report_context(true)
}

/// `run` gathering from the sources `make` builds.
fn with_sources(
    mut run: Run,
    make: impl Fn() -> Vec<Box<dyn ContextSource>> + Send + Sync + 'static,
) -> Run {
    run.external = Some(Arc::new(make));
    run
}

fn item<'a>(row: &'a ContextSourceRecord, id: &str) -> &'a ContextItemRecord {
    row.items
        .iter()
        .find(|i| i.id == id)
        .unwrap_or_else(|| panic!("no `{id}` item in {row:?}"))
}

/// #9194 E4: config asks for no external source: `not_requested`.
#[tokio::test]
async fn no_enabled_external_source_is_not_requested() {
    let ran = Run::new(report()).go().await;
    let external = row(&ran.outcome, "external_sources");
    assert_eq!(
        (external.state, external.detail.as_deref()),
        (
            SourceState::NotRequested,
            Some("no external context source is configured")
        )
    );
    assert!(external.items.is_empty());
}

/// #9194 (ruling Q4): the row is the worst of its sources' items and names
/// the failed one.
#[tokio::test]
async fn external_row_is_worst_of_its_sources_and_names_the_failed_one() {
    let run = with_sources(Run::new(report()), || {
        vec![
            scripted("jira", true, Answer::Snippets(1)),
            scripted("confluence", true, Answer::Error("confluence is down")),
            scripted("pr_history", false, Answer::Snippets(4)),
        ]
    });
    let ran = run.go().await;
    let external = row(&ran.outcome, "external_sources");
    assert_eq!(external.state, SourceState::Unavailable);
    assert!(
        external
            .detail
            .as_deref()
            .unwrap_or_default()
            .contains("confluence"),
        "{external:?}"
    );
    assert_eq!(item(external, "jira").state, SourceState::Used);
    assert!(item(external, "jira").chars > 0);
    let down = item(external, "confluence");
    assert_eq!(down.state, SourceState::Unavailable);
    assert!(
        down.detail
            .as_deref()
            .unwrap_or_default()
            .contains("confluence is down"),
        "{down:?}"
    );
    assert_eq!(external.items.len(), 2, "a disabled source gets no item");
}

/// #9194 amendment 2 (ruling R2): a source config explicitly enables that
/// is disabled (no credentials, or no transport) is `unavailable`.
#[tokio::test]
async fn configured_but_disabled_source_is_unavailable() {
    let mut run = Run::new(report());
    run.config.context_sources.jira.enabled = Some(true);
    let run = with_sources(run, || vec![scripted("jira", false, Answer::Snippets(1))]);
    let ran = run.go().await;
    let external = row(&ran.outcome, "external_sources");
    assert_eq!(external.state, SourceState::Unavailable);
    let jira = item(external, "jira");
    assert_eq!(
        (jira.state, jira.detail.as_deref()),
        (
            SourceState::Unavailable,
            Some("enabled in config but disabled: credentials or transport missing")
        )
    );
}

/// #9194 E6: a long, multi-line error is one line of at most 200 characters.
#[tokio::test]
async fn detail_is_bounded_and_single_line() {
    const LONG: &str = "first line of a long failure\nsecond line\r\n\tthird line \
                        xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\
                        xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\
                        xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx";
    let run = with_sources(Run::new(report()), || {
        vec![scripted("jira", true, Answer::Error(LONG))]
    });
    let ran = run.go().await;
    let external = row(&ran.outcome, "external_sources");
    let detail = item(external, "jira").detail.clone().unwrap_or_default();
    assert!(
        detail.chars().count() <= 200,
        "{} chars",
        detail.chars().count()
    );
    assert!(!detail.contains(['\n', '\r', '\t']), "{detail:?}");
    assert!(
        detail.starts_with("jira API returned 500: first line"),
        "{detail:?}"
    );
}

/// #9194 R3: a bearer token in a source's error never reaches a detail.
#[tokio::test]
async fn a_bearer_token_never_reaches_a_detail() {
    const ERR: &str = "401 Unauthorized; request had Authorization: Bearer s3cr3t-tok \
                       and token=ghp_0123456789abcdefABCDEF0123456789abcd";
    let run = with_sources(Run::new(report()), || {
        vec![scripted("confluence", true, Answer::Error(ERR))]
    });
    let ran = run.go().await;
    let json = serde_json::to_string(&ran.outcome.context_sources).expect("serialises");
    for secret in ["s3cr3t-tok", "ghp_0123456789abcdef", "0123456789abcd"] {
        assert!(!json.contains(secret), "{secret} leaked: {json}");
    }
    let external = row(&ran.outcome, "external_sources");
    let detail = item(external, "confluence")
        .detail
        .clone()
        .unwrap_or_default();
    assert!(detail.contains("401 Unauthorized"), "{detail}");
}

/// #9194 Q8: a failed source changes neither the reviewer's prompt nor the
/// verdict when reporting is turned on.
#[tokio::test]
async fn external_failure_leaves_the_prompt_and_verdict_unchanged() {
    let sources = || {
        vec![
            scripted("jira", true, Answer::Snippets(2)),
            scripted("confluence", true, Answer::Error("down")),
        ]
    };
    let off = with_sources(Run::new(OptionalContextRequest::default()), sources)
        .go()
        .await;
    let on = with_sources(Run::new(report()), sources).go().await;
    assert_eq!(on.reviewer.requests(), off.reviewer.requests());
    let user = &on.reviewer.requests()[0].1;
    assert!(
        user.contains("## Related jira"),
        "the used source reached the prompt"
    );
    assert_eq!(
        serde_json::to_value(&on.outcome.result.verdict).expect("verdict"),
        serde_json::to_value(&off.outcome.result.verdict).expect("verdict")
    );
    assert!(off.outcome.context_sources.is_empty());
}

/// #9194 E5: a local-diff review with `github_issues` enabled makes no
/// GitHub call and reports the source absent.
#[tokio::test]
async fn local_diff_run_reports_github_issues_absent() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (source, _tmp) = local_diff_source(&super::optional_context_off::billing_diff());
    let mut run = Run::new(report());
    run.source = Some(source);
    let seen = calls.clone();
    let run = with_sources(run, move || {
        let issues = GithubIssuesSource::new(
            true,
            RetrievalMode::Live,
            Box::new(Token),
            Box::new(CountingSearch(seen.clone())),
        );
        vec![Box::new(issues) as Box<dyn ContextSource>]
    });
    let ran = run.go().await;
    assert_eq!(calls.load(Ordering::SeqCst), 0, "no GitHub search call");
    let external = row(&ran.outcome, "external_sources");
    let issues = item(external, "github_issues");
    assert_eq!(
        (issues.state, issues.detail.as_deref()),
        (SourceState::Absent, Some("local diff has no repository"))
    );
    assert_eq!(external.state, SourceState::Absent);
}
