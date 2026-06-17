//! Intent/method-conformance context source — the BACK gate (C2, #1359).
//!
//! # Spec References
//!
//! - [`SPEC-CONFORMANCE-02~draft`](docs/specs/intent-conformance.md#SPEC-CONFORMANCE-02~draft)
//!
//! Why: the intent/method-conformance capability (DOC-15) ships a BACK gate in
//! trusty-review that, during PR review, surfaces "what method did the ticket /
//! spec prescribe?" so the reviewer LLM can flag a diff that **explicitly
//! contradicts** that method (matrix M5).  Modelling the gate as a
//! `ContextSource` reuses the entire existing pipeline (gather → prompt → parse →
//! grade) with the documented one-line registration seam (spec §5.2, §7.1) — the
//! source renders the resolved intent as review context; the LLM emits the
//! `method-conformance` finding; the verdict floor caps it at REQUEST_CHANGES.
//!
//! What: `ConformanceSource` implements `ContextSource` by calling the shared ISR
//! ([`trusty_common::intent_source::resolve`], C1 / #1363) with an
//! `IntentQuery::Pr` built from the `ReviewSubject`, then renders the resulting
//! `ResolvedIntent` into a `## Intended method (ticket/spec)` section.  The ISR
//! seams (`TicketFetcher` / `SpecLookup`) are injected so the source is testable
//! offline; the production constructor wires a `BackendTicketFetcher` behind a
//! pluggable token resolver (spec §7.4 — NO hard-wired PAT) and an `FsSpecLookup`.
//!
//! ## Fail-open (AC-11)
//! If the ISR returns `unresolved` (ticket fetch failed) or `none` (no ticket
//! linkage / no intent), `gather` returns an EMPTY section and manufactures NO
//! finding — exactly like every sibling source (spec §5.2, §4.2).  A missing
//! intent source never produces a false-positive conformance finding.
//!
//! ## Auth (spec §7.4)
//! The ISR is reached through a pluggable [`ConformanceTokenResolver`] mirroring
//! `github_issues::IssueTokenResolver`, so it works under review's App/JWT serve
//! mode, not just a `GITHUB_TOKEN` PAT.
//!
//! Test: `conformance_tests.rs` — gather renders the ticket method, fail-open on
//! unresolved/none yields an empty section, semantic-mode errors, disabled-source
//! skip.

use std::sync::Arc;

use async_trait::async_trait;
use trusty_common::intent_source::backend_fetcher::{BackendTicketFetcher, FsSpecLookup};
use trusty_common::intent_source::{
    ChangedFile, IntentQuery, IntentTokenResolver, IsrError, Method, Precedence, ResolvedIntent,
    SpecLookup, TicketFetcher, resolve_default,
};

use super::{
    ContextSection, ContextSnippet, ContextSource, ContextSourceError, RetrievalMode,
    ReviewSubject, SNIPPET_BODY_CHARS, truncate_on_char_boundary,
};
use crate::config::ReviewConfig;
use crate::integrations::github::{AuthStrategy, GithubClient, RunMode};

/// Source identifier used in logs, config keys, and error messages.
const SOURCE_NAME: &str = "conformance";

/// Heading rendered for the resolved-intent section in the reviewer prompt.
const SECTION_HEADING: &str = "Intended method (ticket/spec)";

// ─── Auth seam (mirrors github_issues::IssueTokenResolver, spec §7.4) ─────────

/// Resolves a GitHub bearer token for the ISR's ticket fetch.
///
/// Why: the ISR must NOT hard-wire a PAT (spec §7.4).  This seam mirrors
/// `github_issues::IssueTokenResolver` so the conformance source reuses review's
/// dual-mode auth (CLI → PAT/`gh`, serve → App) and tests can inject a fixed
/// token without `gh`/network.
/// What: one async method returning a token for `owner`, or an error (treated as
/// fail-open skip by `gather`).
/// Test: `DualModeConformanceToken` (prod) + a fake in `conformance_tests`.
#[async_trait]
pub trait ConformanceTokenResolver: Send + Sync {
    /// Resolve a GitHub token for `owner`, or an error (treated as skip).
    async fn resolve(&self, owner: &str) -> Result<String, ContextSourceError>;
}

/// Production resolver delegating to review's #582 dual-mode `AuthStrategy`.
///
/// Why: keeps the single dual-mode auth funnel — the conformance source adds NO
/// new credential path (spec §7.4).
/// What: holds the run mode + a `ReviewConfig` snapshot; `resolve` selects the
/// strategy and resolves a token, mapping any error to `NotConfigured` (skip).
/// Test: not unit-tested directly (needs real auth); the seam is exercised via a
/// fake resolver in `conformance_tests`.
pub struct DualModeConformanceToken {
    run_mode: RunMode,
    config: ReviewConfig,
}

impl DualModeConformanceToken {
    /// Construct from the run mode and a config snapshot.
    ///
    /// Why: the resolver needs the same config the rest of the GitHub path uses.
    /// What: stores `run_mode` and a clone of `config`.
    /// Test: covered transitively by `ConformanceSource::from_config`.
    #[must_use]
    pub fn new(run_mode: RunMode, config: ReviewConfig) -> Self {
        Self { run_mode, config }
    }
}

#[async_trait]
impl ConformanceTokenResolver for DualModeConformanceToken {
    async fn resolve(&self, owner: &str) -> Result<String, ContextSourceError> {
        let client = GithubClient::new().map_err(|e| ContextSourceError::NotConfigured {
            src: SOURCE_NAME,
            reason: format!("failed to build HTTP client: {e}"),
        })?;
        AuthStrategy::select(self.run_mode, None)
            .resolve_token(&client, &self.config, owner)
            .await
            .map_err(|e| ContextSourceError::NotConfigured {
                src: SOURCE_NAME,
                reason: format!("GitHub token unavailable: {e}"),
            })
    }
}

/// Adapter bridging a [`ConformanceTokenResolver`] into the ISR's
/// [`IntentTokenResolver`] seam.
///
/// Why: the ISR's `BackendTicketFetcher` consumes an `IntentTokenResolver`; this
/// adapter lets review's dual-mode resolver feed the ISR without the ISR
/// depending on review's auth types (the dependency only points one way).
/// What: wraps an `Arc<dyn ConformanceTokenResolver>` and implements
/// `IntentTokenResolver::token` by delegating to `resolve`, mapping the
/// review-side error into the ISR's `IsrError::NoToken`.
/// Test: covered transitively by `ConformanceSource::from_config` wiring.
struct IsrTokenBridge {
    inner: Arc<dyn ConformanceTokenResolver>,
}

#[async_trait]
impl IntentTokenResolver for IsrTokenBridge {
    async fn token(&self, owner: &str, _repo: &str) -> Result<String, IsrError> {
        self.inner
            .resolve(owner)
            .await
            .map_err(|e| IsrError::NoToken(e.to_string()))
    }
}

// ─── The source ──────────────────────────────────────────────────────────────

/// BACK-gate context source: surfaces the resolved ticket/spec intent.
///
/// Why: the back gate (#1359) is modelled as a `ContextSource` so it reuses the
/// existing review pipeline (spec §5.2).  It renders the resolved intent for the
/// reviewer LLM; the LLM emits the `method-conformance` finding; `grade.rs` caps
/// it at REQUEST_CHANGES.
/// What: holds `enabled`, `mode`, a boxed `TicketFetcher`, and a `SpecLookup`
/// (both ISR seams, injected for testability).  `gather` builds an
/// `IntentQuery::Pr` from the subject, calls the ISR, and renders the result.
/// Test: `conformance_tests.rs`.
pub struct ConformanceSource {
    enabled: bool,
    mode: RetrievalMode,
    fetcher: Box<dyn TicketFetcher>,
    spec_lookup: Box<dyn SpecLookup>,
}

impl ConformanceSource {
    /// Build from resolved config + run mode, wiring the ISR production seams.
    ///
    /// Why: the runner constructs the source set from config; the conformance
    /// source must wire the ISR's `BackendTicketFetcher` (behind review's
    /// dual-mode token resolver, spec §7.4) and an `FsSpecLookup` rooted at the
    /// process CWD's git root for declared spec-link resolution.
    /// What: computes `effective_enabled(false)` (default DISABLED unless
    /// explicitly enabled — the ISR needs GitHub auth, so we do not auto-enable
    /// on mere credential *presence* the way `github_issues` does), and attaches
    /// the production fetcher + spec lookup.
    /// Test: `from_config_respects_explicit_disable` in `conformance_tests`.
    pub fn from_config(cfg: &super::SourceConfig, run_mode: RunMode, config: ReviewConfig) -> Self {
        let token: Arc<dyn ConformanceTokenResolver> =
            Arc::new(DualModeConformanceToken::new(run_mode, config));
        let bridge = IsrTokenBridge { inner: token };
        let fetcher = Box::new(BackendTicketFetcher::new(Box::new(bridge)));
        let repo_root = crate::config::index_resolver::repo_root_from_cwd();
        let spec_lookup = Box::new(FsSpecLookup::new(repo_root));
        Self {
            // Default DISABLED (conformance needs explicit opt-in + GitHub auth).
            enabled: cfg.effective_enabled(false),
            mode: cfg.mode,
            fetcher,
            spec_lookup,
        }
    }

    /// Construct directly (tests inject ISR seam fakes).
    ///
    /// Why: drive `gather` against mock ISR seams without auth or network.
    /// What: stores the provided fields verbatim.
    /// Test: `gather_renders_ticket_method`, `gather_fail_open_on_unresolved`.
    #[must_use]
    pub fn new(
        enabled: bool,
        mode: RetrievalMode,
        fetcher: Box<dyn TicketFetcher>,
        spec_lookup: Box<dyn SpecLookup>,
    ) -> Self {
        Self {
            enabled,
            mode,
            fetcher,
            spec_lookup,
        }
    }

    /// Build the ISR query from the review subject.
    ///
    /// Why: the ISR's `IntentQuery::Pr` keys ticket linkage off the PR body and
    /// (forward-looking) spec resolution off changed-file content.  The
    /// `ReviewSubject` carries the owner/repo/body/changed-file *paths*; file
    /// *content* is not available at this seam, so spec-link resolution is a gap
    /// for C2 (documented; C4/#1361 hardens the spec axis).  The ticket-method
    /// axis — the primary C2 path — works from the PR body.
    /// What: maps the subject into an `IntentQuery::Pr` with empty branch /
    /// commit-message linkage sources (body-only) and changed files carrying
    /// paths but empty content.  Returns `None` when there is no owner/repo (e.g.
    /// local-diff mode) so `gather` skips with an empty section.
    /// Test: `query_none_without_owner_repo`, `gather_renders_ticket_method`.
    fn build_query(subject: &ReviewSubject) -> Option<IntentQuery> {
        if subject.owner.is_empty() || subject.repo.is_empty() {
            return None;
        }
        let changed_files = subject
            .changed_files
            .iter()
            .map(|p| ChangedFile {
                path: p.clone(),
                content: String::new(),
            })
            .collect();
        Some(IntentQuery::Pr {
            owner: subject.owner.clone(),
            repo: subject.repo.clone(),
            pr_number: 0,
            body: subject.body.clone(),
            branch: None,
            commit_messages: Vec::new(),
            changed_files,
        })
    }

    /// Render a `ResolvedIntent` into a context section (empty when no intent).
    ///
    /// Why: the reviewer LLM needs the resolved method (and which source won,
    /// plus any stale-spec advisory) to judge conformance; an empty section
    /// (dropped by the orchestrator) is the fail-open / gap signal (AC-9, AC-11).
    /// What: returns an empty-snippet section when the intent is `unresolved`, a
    /// gap, or has no method on either axis (M3 — nothing to conform to).
    /// Otherwise renders one snippet for the precedence-winning method and, when
    /// a stale-spec conflict exists (M4), an advisory snippet noting it.
    /// Test: `gather_renders_ticket_method`, `gather_fail_open_on_unresolved`,
    /// `gather_gap_renders_empty`, `render_stale_spec_advisory`.
    fn render_section(intent: &ResolvedIntent) -> ContextSection {
        // Fail-open / gap: unresolved or no method on either axis → empty section.
        if intent.unresolved.is_some() || intent.is_gap() {
            return Self::empty_section();
        }

        let mut snippets: Vec<ContextSnippet> = Vec::new();

        // The precedence-winning method is the thing to check conformance against.
        if let Some(method) = Self::winning_method(intent) {
            let ticket_label = intent
                .ticket
                .as_ref()
                .map(|t| format!("ticket {}", t.id))
                .unwrap_or_else(|| "ticket".to_string());
            let source_label = match intent.precedence_winner {
                Precedence::Ticket => ticket_label,
                Precedence::Spec => intent
                    .spec_section
                    .as_ref()
                    .map(|s| format!("spec {}", s.spec_id))
                    .unwrap_or_else(|| "spec".to_string()),
                Precedence::None => "intent".to_string(),
            };
            let body =
                truncate_on_char_boundary(method.text.trim(), SNIPPET_BODY_CHARS).to_string();
            snippets.push(ContextSnippet {
                title: format!("Prescribed method (from {source_label})"),
                subtitle: Some(
                    "Flag the diff ONLY if it explicitly contradicts this method.".to_string(),
                ),
                body: (!body.is_empty()).then_some(body),
                link: intent.ticket.as_ref().and_then(|t| t.url.clone()),
            });
        }

        // M4: a stale-spec conflict is ADVISORY context, never the thing to fail
        // against — the ticket already won precedence (spec §5.2 precedence wiring).
        if intent.stale_spec
            && let Some(spec_method) = &intent.spec_method
        {
            let body =
                truncate_on_char_boundary(spec_method.text.trim(), SNIPPET_BODY_CHARS).to_string();
            snippets.push(ContextSnippet {
                title: "Conflicting spec method (stale — advisory only)".to_string(),
                subtitle: Some(
                    "The ticket overrides this; do NOT flag the diff for matching the ticket."
                        .to_string(),
                ),
                body: (!body.is_empty()).then_some(body),
                link: None,
            });
        }

        ContextSection {
            heading: SECTION_HEADING.to_string(),
            snippets,
        }
    }

    /// The method to check conformance against, under precedence (ticket > spec).
    ///
    /// Why: the matrix compares the diff against the precedence-winning method
    /// (M1/M2); the renderer must pick the right one.
    /// What: returns the ticket method when the ticket won, the spec method when
    /// the spec won, else `None`.
    /// Test: covered by `gather_renders_ticket_method` / spec-win tests.
    fn winning_method(intent: &ResolvedIntent) -> Option<&Method> {
        match intent.precedence_winner {
            Precedence::Ticket => intent.ticket_method.as_ref(),
            Precedence::Spec => intent.spec_method.as_ref(),
            Precedence::None => None,
        }
    }

    /// An empty section (no snippets) — the fail-open / gap signal.
    ///
    /// Why: the orchestrator drops empty sections, so an empty section is exactly
    /// "no conformance context, manufacture nothing" (AC-9, AC-11).
    /// What: a `ContextSection` with the standard heading and zero snippets.
    /// Test: `gather_fail_open_on_unresolved`, `gather_gap_renders_empty`.
    fn empty_section() -> ContextSection {
        ContextSection {
            heading: SECTION_HEADING.to_string(),
            snippets: Vec::new(),
        }
    }
}

#[async_trait]
impl ContextSource for ConformanceSource {
    fn name(&self) -> &'static str {
        SOURCE_NAME
    }

    fn is_enabled(&self) -> bool {
        self.enabled
    }

    fn mode(&self) -> RetrievalMode {
        self.mode
    }

    async fn gather(&self, subject: &ReviewSubject) -> Result<ContextSection, ContextSourceError> {
        if self.mode == RetrievalMode::Semantic {
            return Err(ContextSourceError::SemanticNotImplemented { src: SOURCE_NAME });
        }
        let Some(query) = Self::build_query(subject) else {
            // No owner/repo (local-diff) — nothing to resolve; empty, not error.
            return Ok(Self::empty_section());
        };
        // The ISR is itself FAIL-OPEN: a fetch/parse failure yields
        // `ResolvedIntent::unresolved` (never an Err), which `render_section`
        // turns into an empty section.  No conformance finding is manufactured
        // (AC-11).
        let intent = resolve_default(query, self.fetcher.as_ref(), self.spec_lookup.as_ref()).await;
        Ok(Self::render_section(&intent))
    }
}

#[cfg(test)]
#[path = "conformance_tests.rs"]
mod tests;
