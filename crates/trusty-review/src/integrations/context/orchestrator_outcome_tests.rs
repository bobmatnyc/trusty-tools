//! What each enabled external source did during a gather (#9194).
//!
//! Why: the gather is fail-open, so an errored, timed-out or empty source
//! contributed nothing and left no trace; the context-source ledger must
//! name each one, and the sections the prompt gets must not change.
//! What: scripted sources through [`gather_external_context_detailed`], and
//! the real `github_issues` source on a local diff.
//! Test: this module.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;

use super::*;
use crate::config::constants::LOCAL_OWNER;
use crate::integrations::context::{
    ContextSnippet, ContextSourceError, GithubIssuesSource, RetrievalMode,
    github_issues::{IssueSearchTransport, IssueTokenResolver},
};

/// A source with a fixed answer.
pub(crate) struct Scripted {
    pub(crate) name: &'static str,
    pub(crate) enabled: bool,
    pub(crate) answer: Answer,
}

/// What a [`Scripted`] source answers.
#[derive(Clone)]
pub(crate) enum Answer {
    /// A section with this many snippets.
    Snippets(usize),
    /// An API error carrying this body.
    Error(&'static str),
    /// Never answers; the orchestrator's timeout must fire.
    Hang,
}

#[async_trait]
impl ContextSource for Scripted {
    fn name(&self) -> &'static str {
        self.name
    }
    fn is_enabled(&self) -> bool {
        self.enabled
    }
    fn mode(&self) -> RetrievalMode {
        RetrievalMode::Live
    }
    async fn gather(&self, _: &ReviewSubject) -> Result<ContextSection, ContextSourceError> {
        match self.answer {
            Answer::Snippets(n) => Ok(section_of(self.name, n)),
            Answer::Error(body) => Err(ContextSourceError::Api {
                src: self.name,
                status: 500,
                body: body.to_string(),
            }),
            Answer::Hang => {
                tokio::time::sleep(Duration::from_secs(3600)).await;
                Ok(section_of(self.name, 0))
            }
        }
    }
}

/// A section headed after `name` with `n` snippets.
pub(crate) fn section_of(name: &str, n: usize) -> ContextSection {
    ContextSection {
        heading: format!("Related {name}"),
        snippets: (0..n)
            .map(|i| ContextSnippet {
                title: format!("{name}-{i}"),
                subtitle: None,
                body: None,
                link: None,
            })
            .collect(),
    }
}

/// A boxed [`Scripted`] source.
pub(crate) fn scripted(
    name: &'static str,
    enabled: bool,
    answer: Answer,
) -> Box<dyn ContextSource> {
    Box::new(Scripted {
        name,
        enabled,
        answer,
    })
}

fn outcome<'a>(gather: &'a ExternalGather, name: &str) -> &'a SourceOutcome {
    gather
        .outcomes
        .iter()
        .find(|o| o.name == name)
        .unwrap_or_else(|| panic!("no `{name}` outcome in {:?}", gather.outcomes))
}

/// #9194 E1: a source that errors is `unavailable`, with its error text.
#[tokio::test]
async fn failed_external_source_is_unavailable_with_its_error() {
    let sources = vec![scripted("jira", true, Answer::Error("jira is down"))];
    let gather = gather_external_context_detailed(&sources, &ReviewSubject::default()).await;
    let jira = outcome(&gather, "jira");
    assert_eq!(jira.state, SourceState::Unavailable);
    let detail = jira.detail.as_deref().unwrap_or_default();
    assert!(detail.contains("jira is down"), "{detail}");
    assert!(gather.sections.is_empty());
}

/// #9194 E2: a source that outlasts the per-source timeout is `unavailable`.
#[tokio::test]
async fn timed_out_external_source_is_unavailable() {
    tokio::time::pause();
    let sources = vec![scripted("confluence", true, Answer::Hang)];
    let gather = gather_external_context_detailed(&sources, &ReviewSubject::default()).await;
    let slow = outcome(&gather, "confluence");
    assert_eq!(slow.state, SourceState::Unavailable);
    assert_eq!(slow.detail.as_deref(), Some("timed out after 20s"));
}

/// #9194 E3: a source that answered with nothing is `absent`, "no results".
#[tokio::test]
async fn empty_external_source_is_absent() {
    let sources = vec![scripted("pr_history", true, Answer::Snippets(0))];
    let gather = gather_external_context_detailed(&sources, &ReviewSubject::default()).await;
    let empty = outcome(&gather, "pr_history");
    assert_eq!(
        (empty.state, empty.detail.as_deref()),
        (SourceState::Absent, Some("no results"))
    );
}

/// #9194: a contributing source is `used`, with its rendered length; a
/// disabled source gets no outcome.
#[tokio::test]
async fn contributing_external_source_is_used_with_chars() {
    let sources = vec![
        scripted("jira", true, Answer::Snippets(2)),
        scripted("confluence", false, Answer::Snippets(3)),
    ];
    let gather = gather_external_context_detailed(&sources, &ReviewSubject::default()).await;
    let jira = outcome(&gather, "jira");
    assert_eq!(jira.state, SourceState::Used);
    assert_eq!(
        jira.chars,
        render_sections(&[section_of("jira", 2)]).chars().count()
    );
    assert_eq!(gather.outcomes.len(), 1, "{:?}", gather.outcomes);
}

/// #9194: the detailed gather hands the prompt exactly the sections the
/// fail-open gather does, in source order.
#[tokio::test]
async fn gather_external_context_still_fails_open_for_the_prompt() {
    tokio::time::pause();
    let sources = || {
        vec![
            scripted("jira", true, Answer::Error("boom")),
            scripted("confluence", true, Answer::Snippets(1)),
            scripted("github_issues", true, Answer::Hang),
            scripted("pr_history", true, Answer::Snippets(2)),
        ]
    };
    let subject = ReviewSubject::default();
    let plain = gather_external_context(&sources(), &subject).await;
    let detailed = gather_external_context_detailed(&sources(), &subject).await;
    assert_eq!(detailed.sections, plain);
    let names: Vec<&str> = detailed.outcomes.iter().map(|o| o.name).collect();
    assert_eq!(names, ["jira", "confluence", "github_issues", "pr_history"]);
}

/// A GitHub search transport that counts its calls.
#[derive(Default)]
pub(crate) struct CountingSearch(pub(crate) Arc<AtomicUsize>);

#[async_trait]
impl IssueSearchTransport for CountingSearch {
    async fn search(&self, _: &str, _: &str, _: u32) -> Result<String, ContextSourceError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(r#"{"items":[]}"#.to_string())
    }
}

/// A token resolver that always has a token.
pub(crate) struct Token;

#[async_trait]
impl IssueTokenResolver for Token {
    async fn resolve(&self, _: &str) -> Result<String, ContextSourceError> {
        Ok("fixture-token".to_string())
    }
}

/// #9194 E5 (amendment 7): on a local diff `github_issues` makes no GitHub
/// call, and its outcome says why it is absent.
#[tokio::test]
async fn local_diff_github_issues_is_absent_with_no_repository() {
    let calls = Arc::new(AtomicUsize::new(0));
    let source = GithubIssuesSource::new(
        true,
        RetrievalMode::Live,
        Box::new(Token),
        Box::new(CountingSearch(calls.clone())),
    );
    let subject = ReviewSubject {
        owner: LOCAL_OWNER.to_string(),
        repo: "stdin".to_string(),
        title: "Fix login".to_string(),
        identifiers: vec!["login".to_string()],
        ..Default::default()
    };
    let sources: Vec<Box<dyn ContextSource>> = vec![Box::new(source)];
    let gather = gather_external_context_detailed(&sources, &subject).await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "no GitHub call for a local diff"
    );
    let issues = outcome(&gather, "github_issues");
    assert_eq!(
        (issues.state, issues.detail.as_deref()),
        (SourceState::Absent, Some("local diff has no repository"))
    );
}
