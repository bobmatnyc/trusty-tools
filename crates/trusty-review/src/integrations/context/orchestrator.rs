//! Context-source orchestrator (Phase 6, #550).
//!
//! Why: the runner should ask "give me all the external context" once and get
//! back ready-to-embed `## Related <source>` markdown, without knowing how many
//! sources exist or how each fetches.  This module owns the fan-out (bounded
//! concurrency), the per-source timeout, the fail-open policy, and the
//! deterministic section assembly — so adding PR-B's APEX source is a one-line
//! registration with zero changes to the runner or prompt builder.
//!
//! ## Fail-open policy (supplementary, NOT required — contrast #590)
//! Every source is best-effort.  A disabled source is skipped; an errored,
//! timed-out, or empty source contributes nothing and the gather continues.  No
//! source failure ever propagates to the caller.  This is the OPPOSITE of the
//! trusty-search/trusty-analyze required gate (#590), where a missing dependency
//! skips the whole review — those are the core value; these are enrichment.
//!
//! What: `gather_external_context` runs all enabled sources concurrently
//! (bounded by `MAX_CONCURRENCY`), each wrapped in a timeout, collects the
//! non-empty sections in a stable order, and returns them.  `render_sections`
//! turns the collected sections into the markdown block appended to the reviewer
//! user message.
//!
//! Test: `gathers_enabled_only`, `fail_open_on_source_error`,
//! `fail_open_on_timeout`, `sections_stable_order`, `render_sections_*` here.

use std::time::Duration;

use futures_util::future::join_all;
use tracing::{debug, warn};

use super::{ContextSection, ContextSource, ReviewSubject};
use crate::{config::constants::LOCAL_OWNER, models::SourceState};

/// Max number of context sources to query concurrently.
///
/// Why: bound the simultaneous outbound connections so a review never opens an
/// unbounded fan-out; four sources today fit comfortably under this.
/// What: a small constant chunk size for the concurrent gather.
const MAX_CONCURRENCY: usize = 4;

/// Per-source wall-clock timeout.
///
/// Why: an enrichment source must never hang the review; a hung source is just
/// "no extra context".  Each source also sets its own client timeout, but this
/// is the orchestrator-level backstop honouring the fail-open contract.
const PER_SOURCE_TIMEOUT: Duration = Duration::from_secs(20);

/// What one enabled source did during a gather (#9194).
///
/// Why: the fail-open gather drops an errored or timed-out source silently;
/// the context-source ledger must name it.
/// What: the source's name, its ledger state (`used`, `absent` or
/// `unavailable`), the rendered characters it contributed, and the raw
/// reason for an `absent` or `unavailable` state.
/// Test: `failed_external_source_is_unavailable_with_its_error`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SourceOutcome {
    /// `ContextSource::name()`.
    pub(crate) name: &'static str,
    /// What happened to it.
    pub(crate) state: SourceState,
    /// Characters of its rendered section, when it contributed.
    pub(crate) chars: usize,
    /// Why it was absent or unavailable; not yet bounded.
    pub(crate) detail: Option<String>,
}

impl SourceOutcome {
    /// An outcome for `name`; an empty `detail` is none.
    fn of(name: &'static str, state: SourceState, chars: usize, detail: &str) -> Self {
        Self {
            name,
            state,
            chars,
            detail: (!detail.is_empty()).then(|| detail.to_string()),
        }
    }
}

/// A gather's sections plus one [`SourceOutcome`] per enabled source (#9194).
#[derive(Debug, Clone, Default)]
pub(crate) struct ExternalGather {
    /// The non-empty sections, in source order.
    pub(crate) sections: Vec<ContextSection>,
    /// One outcome per enabled source, in source order.
    pub(crate) outcomes: Vec<SourceOutcome>,
}

/// Gather context from all enabled sources, fail-open, bounded-concurrent.
///
/// Why: the single entry the runner calls; encapsulates the concurrency,
/// timeout, and fail-open policy so the runner stays a thin orchestration loop.
/// What: filters to `is_enabled()` sources, runs them in concurrency-bounded
/// chunks (each under `PER_SOURCE_TIMEOUT`), logs and drops any error/timeout,
/// drops empty sections, and returns the surviving sections in source order.
/// #9194: the sections of [`gather_external_context_detailed`].
/// Test: `gathers_enabled_only`, `fail_open_on_source_error`,
/// `fail_open_on_timeout`, `sections_stable_order`.
pub async fn gather_external_context(
    sources: &[Box<dyn ContextSource>],
    subject: &ReviewSubject,
) -> Vec<ContextSection> {
    gather_external_context_detailed(sources, subject)
        .await
        .sections
}

/// [`gather_external_context`] plus what each enabled source did (#9194).
///
/// Why: the owner comment of 2026-10-07 requires a failed source to show in
/// the context-source ledger; the fail-open gather only logged it.
/// What: the same gather, recording one [`SourceOutcome`] per enabled source
/// in source order: `used` with its rendered length, `absent` ("no results",
/// or "local diff has no repository" for `github_issues` on a local diff,
/// amendment 7), or `unavailable` with the error or "timed out after 20s".
/// Disabled sources get no outcome. The sections are unchanged.
/// Test: `failed_external_source_is_unavailable_with_its_error`,
/// `timed_out_external_source_is_unavailable`, `empty_external_source_is_absent`,
/// `contributing_external_source_is_used_with_chars`,
/// `gather_external_context_still_fails_open_for_the_prompt`,
/// `local_diff_github_issues_is_absent_with_no_repository`.
pub(crate) async fn gather_external_context_detailed(
    sources: &[Box<dyn ContextSource>],
    subject: &ReviewSubject,
) -> ExternalGather {
    // Index-tagged sections so we can restore source (registration) order after
    // the concurrent gather (which yields in completion order, not input order).
    let mut collected: Vec<(usize, ContextSection)> = Vec::new();
    let mut outcomes: Vec<(usize, SourceOutcome)> = Vec::new(); // #9194

    // Enabled sources, tagged with their original index.
    let enabled: Vec<(usize, &Box<dyn ContextSource>)> = sources
        .iter()
        .enumerate()
        .filter(|(_, s)| s.is_enabled())
        .collect();

    if enabled.is_empty() {
        debug!("no enabled external context sources");
        return ExternalGather::default();
    }

    for chunk in enabled.chunks(MAX_CONCURRENCY) {
        let futs = chunk.iter().map(|(idx, source)| async move {
            let name = source.name();
            let result = tokio::time::timeout(PER_SOURCE_TIMEOUT, source.gather(subject)).await;
            (*idx, name, result)
        });
        for (idx, name, result) in join_all(futs).await {
            let outcome = match result {
                // Completed within the timeout.
                Ok(Ok(section)) if section.snippets.is_empty() => {
                    debug!(source = name, "context source returned no results");
                    SourceOutcome::of(name, SourceState::Absent, 0, empty_detail(name, subject))
                }
                Ok(Ok(section)) => {
                    debug!(
                        source = name,
                        count = section.snippets.len(),
                        "context source contributed"
                    );
                    let chars = render_sections(std::slice::from_ref(&section))
                        .chars()
                        .count();
                    collected.push((idx, section));
                    SourceOutcome::of(name, SourceState::Used, chars, "")
                }
                // The source errored — log + continue (FAIL-OPEN, supplementary).
                Ok(Err(e)) => {
                    warn!(
                        source = name,
                        "context source failed (continuing without it): {e}"
                    );
                    SourceOutcome::of(name, SourceState::Unavailable, 0, &e.to_string())
                }
                // The source timed out — log + continue (FAIL-OPEN).
                Err(_) => {
                    warn!(
                        source = name,
                        timeout_secs = PER_SOURCE_TIMEOUT.as_secs(),
                        "context source timed out (continuing without it)"
                    );
                    let detail = format!("timed out after {}s", PER_SOURCE_TIMEOUT.as_secs());
                    SourceOutcome::of(name, SourceState::Unavailable, 0, &detail)
                }
            };
            outcomes.push((idx, outcome));
        }
    }

    // Restore stable source order (sort by original index) and strip the tag.
    collected.sort_by_key(|(idx, _)| *idx);
    outcomes.sort_by_key(|(idx, _)| *idx);
    ExternalGather {
        sections: collected.into_iter().map(|(_, section)| section).collect(),
        outcomes: outcomes.into_iter().map(|(_, outcome)| outcome).collect(),
    }
}

/// Why an enabled source that answered had nothing (#9194 amendment 7).
fn empty_detail(name: &str, subject: &ReviewSubject) -> &'static str {
    if name == super::github_issues::SOURCE_NAME && subject.owner == LOCAL_OWNER {
        "local diff has no repository"
    } else {
        "no results"
    }
}

/// Render collected sections into the markdown block for the user message.
///
/// Why: the prompt builder appends external context as `## Related …` sections;
/// keeping the rendering here (not in `prompt.rs`) means the prompt builder
/// stays source-agnostic and the bullet format is tested in one place.
/// What: for each non-empty section, emits an `## <heading>` block followed by
/// one bullet per snippet (`- **title** — subtitle ([link](link))\n  body`).
/// Returns an empty string when there are no sections (caller appends nothing).
/// Test: `render_sections_emits_headings_and_bullets`, `render_sections_empty`.
pub fn render_sections(sections: &[ContextSection]) -> String {
    if sections.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    for section in sections {
        if section.snippets.is_empty() {
            continue;
        }
        out.push_str(&format!("## {}\n\n", section.heading));
        for snip in &section.snippets {
            out.push_str("- **");
            out.push_str(&snip.title);
            out.push_str("**");
            if let Some(sub) = &snip.subtitle {
                out.push_str(&format!(" — {sub}"));
            }
            if let Some(link) = &snip.link {
                out.push_str(&format!(" ([link]({link}))"));
            }
            out.push('\n');
            if let Some(body) = &snip.body {
                // Indent the body excerpt under the bullet.
                for line in body.lines() {
                    out.push_str("  ");
                    out.push_str(line);
                    out.push('\n');
                }
            }
        }
        out.push('\n');
    }
    out
}

// ─── Unit tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::context::{ContextSnippet, ContextSourceError, RetrievalMode};
    use async_trait::async_trait;
    use std::time::Duration;

    /// A scripted source returning a fixed outcome, for orchestrator tests.
    struct ScriptedSource {
        name: &'static str,
        enabled: bool,
        outcome: Outcome,
    }

    #[derive(Clone)]
    enum Outcome {
        Section(ContextSection),
        Error,
        Hang,
    }

    #[async_trait]
    impl ContextSource for ScriptedSource {
        fn name(&self) -> &'static str {
            self.name
        }
        fn is_enabled(&self) -> bool {
            self.enabled
        }
        fn mode(&self) -> RetrievalMode {
            RetrievalMode::Live
        }
        async fn gather(
            &self,
            _subject: &ReviewSubject,
        ) -> Result<ContextSection, ContextSourceError> {
            match &self.outcome {
                Outcome::Section(s) => Ok(s.clone()),
                Outcome::Error => Err(ContextSourceError::Api {
                    src: self.name,
                    status: 500,
                    body: "boom".to_string(),
                }),
                Outcome::Hang => {
                    // Sleep longer than the per-source timeout to exercise the
                    // timeout fail-open path. Uses tokio's paused-time fast path.
                    tokio::time::sleep(Duration::from_secs(3600)).await;
                    unreachable!("should be cancelled by timeout")
                }
            }
        }
    }

    fn section(heading: &str, n: usize) -> ContextSection {
        ContextSection {
            heading: heading.to_string(),
            snippets: (0..n)
                .map(|i| ContextSnippet {
                    title: format!("item{i}"),
                    subtitle: None,
                    body: None,
                    link: None,
                })
                .collect(),
        }
    }

    fn boxed(s: ScriptedSource) -> Box<dyn ContextSource> {
        Box::new(s)
    }

    #[tokio::test]
    async fn gathers_enabled_only() {
        let sources = vec![
            boxed(ScriptedSource {
                name: "a",
                enabled: true,
                outcome: Outcome::Section(section("A", 2)),
            }),
            boxed(ScriptedSource {
                name: "b",
                enabled: false, // disabled — must be skipped
                outcome: Outcome::Section(section("B", 5)),
            }),
        ];
        let out = gather_external_context(&sources, &ReviewSubject::default()).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].heading, "A");
    }

    #[tokio::test]
    async fn fail_open_on_source_error() {
        // One source errors, one succeeds → the error is swallowed, the good
        // section survives. The review is NOT blocked.
        let sources = vec![
            boxed(ScriptedSource {
                name: "bad",
                enabled: true,
                outcome: Outcome::Error,
            }),
            boxed(ScriptedSource {
                name: "good",
                enabled: true,
                outcome: Outcome::Section(section("Good", 1)),
            }),
        ];
        let out = gather_external_context(&sources, &ReviewSubject::default()).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].heading, "Good");
    }

    #[tokio::test]
    async fn fail_open_on_timeout() {
        // Pause tokio's clock so the 20s per-source timeout fires instantly when
        // the orchestrator's `sleep`-based timeout is auto-advanced; the hanging
        // source must be dropped (timeout), not block the gather.
        tokio::time::pause();
        let sources = vec![
            boxed(ScriptedSource {
                name: "slow",
                enabled: true,
                outcome: Outcome::Hang,
            }),
            boxed(ScriptedSource {
                name: "fast",
                enabled: true,
                outcome: Outcome::Section(section("Fast", 1)),
            }),
        ];
        let out = gather_external_context(&sources, &ReviewSubject::default()).await;
        assert_eq!(out.len(), 1, "hanging source dropped via timeout");
        assert_eq!(out[0].heading, "Fast");
    }

    #[tokio::test]
    async fn empty_sections_dropped() {
        let sources = vec![boxed(ScriptedSource {
            name: "empty",
            enabled: true,
            outcome: Outcome::Section(section("Empty", 0)),
        })];
        let out = gather_external_context(&sources, &ReviewSubject::default()).await;
        assert!(out.is_empty(), "empty section contributes nothing");
    }

    #[tokio::test]
    async fn sections_stable_order() {
        // Even if the second source were faster, output order follows input
        // (source registration) order so the prompt is deterministic.
        let sources = vec![
            boxed(ScriptedSource {
                name: "first",
                enabled: true,
                outcome: Outcome::Section(section("First", 1)),
            }),
            boxed(ScriptedSource {
                name: "second",
                enabled: true,
                outcome: Outcome::Section(section("Second", 1)),
            }),
            boxed(ScriptedSource {
                name: "third",
                enabled: true,
                outcome: Outcome::Section(section("Third", 1)),
            }),
        ];
        let out = gather_external_context(&sources, &ReviewSubject::default()).await;
        let headings: Vec<&str> = out.iter().map(|s| s.heading.as_str()).collect();
        assert_eq!(headings, vec!["First", "Second", "Third"]);
    }

    #[test]
    fn render_sections_empty() {
        assert_eq!(render_sections(&[]), "");
        // A section whose snippets are empty renders nothing.
        assert_eq!(render_sections(&[section("Empty", 0)]), "");
    }

    #[test]
    fn render_sections_emits_headings_and_bullets() {
        let sec = ContextSection {
            heading: "Related JIRA tickets".to_string(),
            snippets: vec![ContextSnippet {
                title: "PROJ-1 — Add auth".to_string(),
                subtitle: Some("In Progress".to_string()),
                body: None,
                link: Some("https://acme.atlassian.net/browse/PROJ-1".to_string()),
            }],
        };
        let md = render_sections(&[sec]);
        assert!(md.contains("## Related JIRA tickets"));
        assert!(md.contains("- **PROJ-1 — Add auth** — In Progress"));
        assert!(md.contains("([link](https://acme.atlassian.net/browse/PROJ-1))"));
    }

    #[test]
    fn render_sections_includes_body_indented() {
        let sec = ContextSection {
            heading: "Related GitHub issues".to_string(),
            snippets: vec![ContextSnippet {
                title: "#42 — Bug".to_string(),
                subtitle: Some("open".to_string()),
                body: Some("first line\nsecond line".to_string()),
                link: None,
            }],
        };
        let md = render_sections(&[sec]);
        assert!(md.contains("  first line"));
        assert!(md.contains("  second line"));
    }
}

// #9194: per-source outcomes for the context-source ledger.
#[cfg(test)]
#[path = "orchestrator_outcome_tests.rs"]
pub(crate) mod outcome_tests;
