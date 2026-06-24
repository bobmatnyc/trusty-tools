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
//! `method-conformance` finding; `grade.rs` caps it at REQUEST_CHANGES.
//!
//! ## Test-plan & AC conformance (#1418)
//! `gather` also parses the PR body for a `## Test plan` section and fetches
//! the linked ticket body for `## Acceptance Criteria` / `- [ ]` checklist
//! items.  Items that reference test/verify behaviour with no matching test
//! file or test identifier in the diff are surfaced as "possibly unmet"
//! snippets.  When neither a test-plan section nor AC bullets are found (but
//! the PR body is non-empty), a single advisory snippet is emitted.  All of
//! this is pure string parsing — no extra network calls beyond what the
//! existing ISR ticket fetch already performs.
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
//! skip, test-plan extraction, AC extraction, unmet-AC rendering, advisory on
//! missing plan.

use std::sync::Arc;

use async_trait::async_trait;
use trusty_common::intent_source::backend_fetcher::{BackendTicketFetcher, FsSpecLookup};
use trusty_common::intent_source::{
    ChangedFile, IntentQuery, IntentTokenResolver, IsrError, Method, Precedence, ResolvedIntent,
    SpecLookup, TicketFetcher, extract_pr_ticket, resolve_default,
};

use super::{
    ContextSection, ContextSnippet, ContextSource, ContextSourceError, RetrievalMode,
    ReviewSubject, SNIPPET_BODY_CHARS, truncate_on_char_boundary,
};
use crate::config::ReviewConfig;
use crate::integrations::context::external_spec::ExternalRepoSpecLookup;
use crate::integrations::github::{AuthStrategy, GithubClient, RunMode};

/// Source identifier used in logs, config keys, and error messages.
const SOURCE_NAME: &str = "conformance";

/// Heading rendered for the resolved-intent section in the reviewer prompt.
const SECTION_HEADING: &str = "Intended method (ticket/spec)";

/// Heading rendered when only test-plan gaps are surfaced (#1418).
const GAPS_HEADING: &str = "Test plan gaps";

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
    /// HTTP client built ONCE at construction (mirrors sibling sources, which
    /// build their `GithubClient` once rather than per call).
    ///
    /// `GithubClient::new()` can fail only on TLS-backend init; that failure is
    /// captured here as the `Err` string so `resolve` surfaces the same fail-open
    /// `NotConfigured` skip it did when it built the client per call — without
    /// reconstructing the client (and its connection pool) on every `resolve`.
    client: Result<GithubClient, String>,
}

impl DualModeConformanceToken {
    /// Construct from the run mode and a config snapshot.
    ///
    /// Why: the resolver needs the same config the rest of the GitHub path uses,
    /// and should reuse ONE `GithubClient` (connection pool) across calls instead
    /// of rebuilding it on every `resolve` — matching the sibling sources.
    /// What: stores `run_mode`, a clone of `config`, and a `GithubClient` built
    /// once (its rare TLS-init failure is captured as an `Err` string so the
    /// infallible `from_config` path stays infallible and `resolve` reports the
    /// failure as a fail-open skip).
    /// Test: covered transitively by `ConformanceSource::from_config`.
    #[must_use]
    pub fn new(run_mode: RunMode, config: ReviewConfig) -> Self {
        let client = GithubClient::new().map_err(|e| format!("failed to build HTTP client: {e}"));
        Self {
            run_mode,
            config,
            client,
        }
    }
}

#[async_trait]
impl ConformanceTokenResolver for DualModeConformanceToken {
    async fn resolve(&self, owner: &str) -> Result<String, ContextSourceError> {
        let client = self
            .client
            .as_ref()
            .map_err(|reason| ContextSourceError::NotConfigured {
                src: SOURCE_NAME,
                reason: reason.clone(),
            })?;
        AuthStrategy::select(self.run_mode, None)
            .resolve_token(client, &self.config, owner)
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

// ─── Test-plan / AC parsing helpers (#1418) ───────────────────────────────────

/// Extract bullet items from the `## Test plan` section of a PR body.
///
/// Why: PR authors list what they manually tested or expect CI to verify under
/// `## Test plan`; surfacing those items lets the reviewer flag ones with no
/// corresponding test change in the diff (#1418 AC-1).
/// What: finds the first heading matching `(?i)## test plan`, then collects
/// every leading-`-` / `*` bullet line until the next `##` heading or
/// end-of-body.  Returns an empty `Vec` when no matching section is found.
/// Test: `test_plan_extraction_from_pr_body` in `conformance_tests.rs`.
pub fn extract_test_plan_items(pr_body: &str) -> Vec<String> {
    let mut in_section = false;
    let mut items: Vec<String> = Vec::new();
    for line in pr_body.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("##") {
            let heading_text = trimmed.trim_start_matches('#').trim();
            let normalised = heading_text
                .to_lowercase()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let is_testplan = normalised.starts_with("test plan");
            if is_testplan {
                in_section = true;
                continue;
            }
            if in_section {
                break;
            }
        }
        if in_section && let Some(item) = parse_bullet(trimmed) {
            items.push(item);
        }
    }
    items
}

/// Extract acceptance-criteria bullets from a ticket body.
///
/// Why: ticket AC items state what the diff must implement/prove; items with no
/// test coverage in the diff are "possibly unmet" (#1418 AC-2).
/// What: collects `- [ ]` / `* [ ]` checkbox items from anywhere in the body,
/// plus plain bullets under a case-insensitive `## Acceptance Criteria` section.
/// Returns the union, deduped preserving order, trimmed.
/// Test: `ac_extraction_from_ticket` in `conformance_tests.rs`.
pub fn extract_ac_bullets(ticket_body: &str) -> Vec<String> {
    let mut items: Vec<String> = Vec::new();
    let mut in_ac_section = false;

    for line in ticket_body.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with("##") {
            let heading_text = trimmed.trim_start_matches('#').trim().to_lowercase();
            in_ac_section =
                heading_text.contains("acceptance") && heading_text.contains("criteria");
            continue;
        }

        // Always collect `- [ ]` / `* [ ]` checkbox items regardless of section.
        if trimmed.starts_with("- [ ]") || trimmed.starts_with("* [ ]") {
            let text = trimmed
                .trim_start_matches(['-', '*'])
                .trim()
                .trim_start_matches("[ ]")
                .trim()
                .to_string();
            if !text.is_empty() && !items.contains(&text) {
                items.push(text);
            }
            continue;
        }

        if in_ac_section
            && let Some(item) = parse_bullet(trimmed)
            && !items.contains(&item)
        {
            items.push(item);
        }
    }
    items
}

/// Parse a single leading-bullet line, returning its trimmed text (or `None`).
///
/// Why: both `extract_test_plan_items` and `extract_ac_bullets` need to strip
/// `-`, `*`, `+` leaders and optional checkbox markers; a shared helper avoids
/// drift between the two parsers.
/// What: strips a leading `-`, `*`, or `+` leader (plus any following
/// whitespace including tabs), then any `[x]`/`[X]`/`[ ]` checkbox marker,
/// and returns the remaining text when non-empty.  Using `trim_start` after
/// the leader makes this consistent with the checkbox branch in
/// `extract_ac_bullets` (which already calls `trim_start_matches(['-','*'])`)
/// and handles tab-separated bullets (common in some editors).
/// Test: exercised transitively by the two extraction functions;
/// `parse_bullet_accepts_tab_separator` covers the tab case explicitly.
fn parse_bullet(trimmed: &str) -> Option<String> {
    // Strip the leading marker character, then any following whitespace.
    let after_marker = trimmed
        .strip_prefix('-')
        .or_else(|| trimmed.strip_prefix('*'))
        .or_else(|| trimmed.strip_prefix('+'))
        .map(str::trim_start)?;
    // Strip optional `[x]`/`[X]`/`[ ]` checkbox marker (with trailing whitespace).
    let text = if let Some(rest) = after_marker
        .strip_prefix("[x]")
        .or_else(|| after_marker.strip_prefix("[X]"))
        .or_else(|| after_marker.strip_prefix("[ ]"))
    {
        rest.trim_start()
    } else {
        after_marker
    };
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

/// Returns `true` when `item` references testable / verifiable behaviour.
///
/// Why: not every AC or test-plan item is test-measurable; items like
/// "document the new endpoint" should not trigger a test-gap finding.
/// What: checks whether the lowercased item text contains any of a set of
/// test-signalling keywords: test, verify, check, assert, should, validate,
/// ensure, confirm.
/// Test: exercised transitively by `unmet_ac_renders_snippet`.
fn item_is_test_related(item: &str) -> bool {
    let lower = item.to_lowercase();
    let keywords = [
        "test", "verify", "check", "assert", "should", "validate", "ensure", "confirm",
    ];
    keywords.iter().any(|kw| lower.contains(kw))
}

/// Returns `true` when the diff already covers `item` with a test artifact.
///
/// Why: an AC item is only "possibly unmet" when there is NO evidence of test
/// coverage for THAT SPECIFIC ITEM in the diff; an item should only be
/// considered covered when the test artifact is plausibly related to it, not
/// when any unrelated test file was touched (#1418 review fix).
/// What: extracts significant words (length > 3, excluding common stop-words)
/// from the item text, then checks whether any test-file path or any test
/// identifier contains at least one of those key terms (case-insensitive).
/// A "test artifact" is a file whose path contains "test" or "spec", or an
/// identifier that contains "test".  The heuristic is permissive — a single
/// key-term match counts as covered — to avoid false positives.
/// Test: `item_has_test_coverage_per_item` and transitively
/// `unmet_ac_renders_snippet` in `conformance_tests.rs`.
fn item_has_test_coverage(item: &str, changed_files: &[String], identifiers: &[String]) -> bool {
    // Collect test-file paths and test identifiers from the diff.
    let test_files: Vec<String> = changed_files
        .iter()
        .filter(|f| {
            let lower = f.to_lowercase();
            lower.contains("test") || lower.contains("spec")
        })
        .map(|f| f.to_lowercase())
        .collect();

    let test_ids: Vec<String> = identifiers
        .iter()
        .filter(|id| id.to_lowercase().contains("test"))
        .map(|id| id.to_lowercase())
        .collect();

    if test_files.is_empty() && test_ids.is_empty() {
        return false;
    }

    // Extract significant key terms from the item text (skip short words and
    // common stop-words that would match trivially).
    let stop_words = [
        "the", "and", "for", "with", "that", "this", "from", "are", "not", "have", "been",
        "should", "must", "will", "when", "does", "into", "each", "item", "test", "check",
        "verify", "ensure", "assert", "validate", "confirm",
    ];
    let key_terms: Vec<String> = item
        .split(|c: char| !c.is_alphanumeric())
        .map(|w| w.to_lowercase())
        .filter(|w| w.len() > 3 && !stop_words.contains(&w.as_str()))
        .collect();

    if key_terms.is_empty() {
        // No distinguishing terms — cannot confirm per-item coverage.
        return false;
    }

    // A single key-term hit in any test file path or test identifier counts.
    for term in &key_terms {
        if test_files.iter().any(|f| f.contains(term.as_str())) {
            return true;
        }
        if test_ids.iter().any(|id| id.contains(term.as_str())) {
            return true;
        }
    }

    false
}

/// Build gap snippets from unmet test-plan / AC items (#1418).
///
/// Why: the reviewer LLM needs structured, citable context to raise
/// `test-coverage` findings; citing the exact AC item and its source is
/// actionable (#1413 top Duetto signal #4).
/// What: for every test-related item in `tp_items` and `ac_items` with no
/// per-item test coverage in the diff, emits one `ContextSnippet` titled
/// `"Unmet AC: <item>"` with a source subtitle.  Returns an empty `Vec` when
/// all test-related items are covered OR both lists are empty.
/// Note: callers are expected to only invoke this when at least one list is
/// non-empty; passing both empty lists returns an empty Vec (no advisory is
/// emitted — the advisory was removed because `gather_gap_snippets` already
/// gates on having something to check before calling this function, so the
/// advisory branch was dead code in production).
/// Test: `unmet_ac_renders_snippet`, `empty_body_no_snippets` in
/// `conformance_tests.rs`.
pub fn build_gap_snippets(
    tp_items: &[String],
    ac_items: &[String],
    ticket_url: Option<&str>,
    ticket_id: Option<&str>,
    changed_files: &[String],
    identifiers: &[String],
) -> Vec<ContextSnippet> {
    let mut snippets: Vec<ContextSnippet> = Vec::new();

    for item in tp_items {
        if !item_is_test_related(item) {
            continue;
        }
        if item_has_test_coverage(item, changed_files, identifiers) {
            continue;
        }
        snippets.push(ContextSnippet {
            title: format!("Unmet AC: {}", truncate_on_char_boundary(item, 80)),
            subtitle: Some("PR body test plan".to_string()),
            body: Some(item.clone()),
            link: None,
        });
    }

    let ticket_subtitle = match (ticket_id, ticket_url) {
        (Some(id), Some(url)) => format!("{id} — {url}"),
        (Some(id), None) => id.to_string(),
        _ => "linked ticket".to_string(),
    };
    for item in ac_items {
        if !item_is_test_related(item) {
            continue;
        }
        if item_has_test_coverage(item, changed_files, identifiers) {
            continue;
        }
        snippets.push(ContextSnippet {
            title: format!("Unmet AC: {}", truncate_on_char_boundary(item, 80)),
            subtitle: Some(ticket_subtitle.clone()),
            body: Some(item.clone()),
            link: ticket_url.map(str::to_string),
        });
    }

    snippets
}

// ─── Fallback spec lookup ────────────────────────────────────────────────────

/// Tries `primary`, falls back to `secondary` when primary returns `None`.
///
/// Why: the external-spec lookup (primary) augments rather than replaces the
/// local `FsSpecLookup` (secondary); a fallback chain keeps both active (#1419).
/// What: `load` calls `primary.load(f)` first; returns that if `Some`, else
/// calls `secondary.load(f)`.
/// Test: covered transitively by `from_config` tests in `conformance_tests.rs`.
struct FallbackSpecLookup {
    primary: Box<dyn SpecLookup>,
    secondary: Box<dyn SpecLookup>,
}

impl SpecLookup for FallbackSpecLookup {
    fn load(&self, spec_file: &str) -> Option<String> {
        self.primary
            .load(spec_file)
            .or_else(|| self.secondary.load(spec_file))
    }
}

// ─── The source ──────────────────────────────────────────────────────────────

/// BACK-gate context source: surfaces the resolved ticket/spec intent and
/// test-plan / AC conformance gaps (#1359, #1418).
///
/// Why: the back gate (#1359) is modelled as a `ContextSource` so it reuses the
/// existing review pipeline (spec §5.2).  It renders the resolved intent for the
/// reviewer LLM; the LLM emits the `method-conformance` finding; `grade.rs` caps
/// it at REQUEST_CHANGES.  Since #1418 it also surfaces unmet test-plan / AC
/// items as `test-coverage` findings so the LLM can cite the exact source line.
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
    pub fn from_config(
        cfg: &super::ConformanceSourceConfig,
        run_mode: RunMode,
        config: ReviewConfig,
    ) -> Self {
        let token: Arc<dyn ConformanceTokenResolver> =
            Arc::new(DualModeConformanceToken::new(run_mode, config));
        let bridge = IsrTokenBridge { inner: token };
        let fetcher = Box::new(BackendTicketFetcher::new(Box::new(bridge)));
        let repo_root = crate::config::index_resolver::repo_root_from_cwd();
        let fs_lookup: Box<dyn SpecLookup> = Box::new(FsSpecLookup::new(repo_root));

        // When external_spec_repo is configured, wrap it in a fallback chain
        // (external first, then local FS). Fail-open: if from_parts returns None
        // (malformed repo string or TLS init failure), fall back to fs_lookup only.
        let spec_lookup: Box<dyn SpecLookup> = if let Some(ref owner_repo) = cfg.external_spec_repo
        {
            let token_str = std::env::var("GITHUB_TOKEN").unwrap_or_default();
            match ExternalRepoSpecLookup::from_parts(
                owner_repo,
                cfg.spec_path_prefix.clone(),
                token_str,
            ) {
                Some(ext) => Box::new(FallbackSpecLookup {
                    primary: Box::new(ext),
                    secondary: fs_lookup,
                }),
                None => fs_lookup,
            }
        } else {
            fs_lookup
        };

        Self {
            // Default DISABLED (conformance needs explicit opt-in + GitHub auth).
            enabled: cfg.effective_enabled(false),
            mode: cfg.mode(),
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
    /// `ReviewSubject` carries the owner/repo/body/PR-number/changed-file *paths*;
    /// file *content* is not available at this seam, so spec-link resolution is a
    /// gap for C2 (documented; C4/#1361 hardens the spec axis).  The ticket-method
    /// axis — the primary C2 path — works from the PR body.
    /// What: maps the subject into an `IntentQuery::Pr` with the real PR number
    /// (threaded from the runner; `0` only in local-diff mode), empty branch /
    /// commit-message linkage sources (body-only), and changed files carrying
    /// paths but empty content.  Returns `None` when there is no owner/repo (e.g.
    /// local-diff mode) so `gather` skips with an empty section.
    /// Test: `query_none_without_owner_repo`, `query_carries_pr_number`,
    /// `gather_renders_ticket_method`.
    fn build_query(subject: &ReviewSubject) -> Option<IntentQuery> {
        if subject.owner.is_empty() || subject.repo.is_empty() {
            return None;
        }
        let changed_files = subject
            .changed_files
            .iter()
            .map(|p| ChangedFile {
                // FIXME(C4/#1361): changed-file *content* is not available at this
                // seam, so the spec axis cannot resolve declared spec links yet;
                // PR-body → ticket linkage is the primary C2 path. C4 plumbs file
                // content (and any remaining spec-axis hardening) through here.
                path: p.clone(),
                content: String::new(),
            })
            .collect();
        Some(IntentQuery::Pr {
            owner: subject.owner.clone(),
            repo: subject.repo.clone(),
            // Threaded from the runner (#1359); `0` only in local-diff mode where
            // no PR exists. `build_query` already returns `None` for local diffs
            // (empty owner/repo), so a live query always carries the real number.
            pr_number: subject.pr_number,
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
            // Build the human-readable source label AND a bare citation key the
            // LLM can copy verbatim into `source_citation` (#1419).
            let (source_label, citation_key) = match intent.precedence_winner {
                Precedence::Ticket => {
                    let id = intent
                        .ticket
                        .as_ref()
                        .map(|t| t.id.clone())
                        .unwrap_or_else(|| "unknown".to_string());
                    (format!("ticket {id}"), id)
                }
                Precedence::Spec => {
                    let spec_id = intent
                        .spec_section
                        .as_ref()
                        .map(|s| s.spec_id.clone())
                        .unwrap_or_else(|| "unknown".to_string());
                    (format!("spec {spec_id}"), spec_id)
                }
                Precedence::None => ("intent".to_string(), String::new()),
            };
            let body =
                truncate_on_char_boundary(method.text.trim(), SNIPPET_BODY_CHARS).to_string();
            // Subtitle carries the copyable citation so the LLM can populate
            // `source_citation` with the exact identifier (#1419).
            let subtitle = if citation_key.is_empty() {
                "Flag the diff ONLY if it explicitly contradicts this method.".to_string()
            } else {
                format!(
                    "Flag the diff ONLY if it explicitly contradicts this method. \
                     Source citation key: {citation_key}"
                )
            };
            snippets.push(ContextSnippet {
                title: format!("Prescribed method (from {source_label})"),
                subtitle: Some(subtitle),
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

    /// Whether the BACK gate would surface a method to flag the diff against.
    ///
    /// Why: the cross-gate golden test (AC-18, spec §8) must assert that the SAME
    /// `ResolvedIntent` drives the FRONT gate (`trusty_mpm`) and the BACK gate
    /// (here) to *consistent* matrix outcomes — FRONT `Escalate` ⇔ BACK
    /// would-flag (M5), FRONT `AutoAccept` ⇔ BACK no-finding (M3). The back gate
    /// never emits a conformance finding unless it first surfaces a prescribed
    /// method for the reviewer LLM to check the diff against; a gap / unresolved
    /// intent renders an empty section and can produce no finding (spec §5.2,
    /// §4.2). This predicate exposes exactly that "is there an intent to flag
    /// against?" signal — derived from the *real* renderer ([`render_section`]),
    /// not a reimplementation — so a cross-crate test can drive the genuine gate
    /// code path against a shared intent.
    /// What: returns `true` iff [`render_section`] produces a non-empty section
    /// (i.e. the precedence-winning method exists and is surfaced); `false` for a
    /// gap (M3), an `unresolved` fail-open intent, or any intent with no method on
    /// either axis. This is a necessary condition for a back-gate finding: the
    /// LLM is instructed to flag *only* an explicit contradiction of a surfaced
    /// method, so no surfaced method ⇒ no possible conformance finding.
    ///
    /// [`render_section`]: ConformanceSource::render_section
    /// Test: `would_flag_*` in `conformance_tests.rs`; the cross-gate AC-18 golden
    /// test in `trusty-mpm/tests/conformance_cross_gate.rs`.
    #[must_use]
    pub fn would_flag(intent: &ResolvedIntent) -> bool {
        !Self::render_section(intent).snippets.is_empty()
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

    /// Gather test-plan gap snippets for the diff, fail-open (#1418).
    ///
    /// Why: the gap check runs inside `gather` and must be fail-open; wrapping it
    /// in a dedicated helper keeps `gather` linear and the logic independently
    /// testable via `build_gap_snippets`.
    /// What: parses the PR body for test-plan items; optionally fetches the linked
    /// ticket for AC bullets (same fail-open contract — any fetch error yields an
    /// empty AC list); calls `build_gap_snippets` to produce `ContextSnippet`s.
    /// The advisory snippet ("No test plan or AC found") is only emitted when there
    /// IS a ticket link in the PR body (we know the author linked a ticket, so they
    /// had an opportunity to add AC) — it is suppressed when there is no ticket and
    /// no test-plan section (avoids false-positive advisory for un-ticketed PRs).
    /// Returns an empty `Vec` when the PR body is blank or there is nothing to
    /// check.
    /// Test: `unmet_ac_renders_snippet`, `no_plan_produces_advisory_snippet`,
    /// `empty_body_no_snippets`.
    async fn gather_gap_snippets(&self, subject: &ReviewSubject) -> Vec<ContextSnippet> {
        if subject.body.trim().is_empty() {
            return Vec::new();
        }

        let tp_items = extract_test_plan_items(&subject.body);

        let (ac_items, ticket_url, ticket_id) =
            if !subject.owner.is_empty() && !subject.repo.is_empty() {
                if let Some(id) = extract_pr_ticket(&subject.body, &[], None) {
                    match self.fetcher.fetch(&subject.owner, &subject.repo, &id).await {
                        Ok(data) => {
                            let url = data.url.clone();
                            let tid = data.id.clone();
                            (extract_ac_bullets(&data.body), url, Some(tid))
                        }
                        Err(_) => (Vec::new(), None, None),
                    }
                } else {
                    (Vec::new(), None, None)
                }
            } else {
                (Vec::new(), None, None)
            };

        // Only call `build_gap_snippets` when there are actual items to evaluate.
        // The `build_gap_snippets` advisory (both lists empty) is for direct
        // callers who have already confirmed context; `gather_gap_snippets` is
        // fail-open and silently returns empty when no test-plan section and no
        // AC were found — this avoids false-positive advisories for:
        //   • fetch failures (AC-11 fail-open),
        //   • tickets with no AC (M3 / AC-9 gap → empty),
        //   • PRs without explicit test plans or ticket linkage.
        if tp_items.is_empty() && ac_items.is_empty() {
            return Vec::new();
        }

        build_gap_snippets(
            &tp_items,
            &ac_items,
            ticket_url.as_deref(),
            ticket_id.as_deref(),
            &subject.changed_files,
            &subject.identifiers,
        )
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

    /// Gather intent + test-plan gap context for the PR.
    ///
    /// Why: the conformance source contributes two kinds of reviewer context:
    /// (1) the resolved ticket/spec method for method-conformance checking, and
    /// (2) unmet test-plan / AC items for test-coverage checking (#1418).
    /// Combining them in one gather keeps the source count stable and reuses the
    /// same fail-open orchestrator path.
    /// What: resolves the intent (existing ISR path); gathers gap snippets from
    /// PR-body test-plan + linked-ticket AC; returns a combined section whose
    /// heading is `SECTION_HEADING` when intent snippets are present, or
    /// `GAPS_HEADING` when only gap snippets exist.  An empty section is returned
    /// (orchestrator drops it) when both the intent and the gaps are empty.
    /// Test: existing tests (method intent) + new gap tests in `conformance_tests.rs`.
    async fn gather(&self, subject: &ReviewSubject) -> Result<ContextSection, ContextSourceError> {
        if self.mode == RetrievalMode::Semantic {
            return Err(ContextSourceError::SemanticNotImplemented { src: SOURCE_NAME });
        }

        // Intent section (existing ISR path, fail-open).
        let intent_snippets = if let Some(query) = Self::build_query(subject) {
            // The ISR is itself FAIL-OPEN: a fetch/parse failure yields
            // `ResolvedIntent::unresolved` (never an Err), which `render_section`
            // turns into an empty section.  No conformance finding is manufactured
            // (AC-11).
            let intent =
                resolve_default(query, self.fetcher.as_ref(), self.spec_lookup.as_ref()).await;
            Self::render_section(&intent).snippets
        } else {
            Vec::new()
        };
        let has_intent = !intent_snippets.is_empty();

        // Test-plan / AC gaps (fail-open — any error yields an empty Vec).
        let gap_snippets = self.gather_gap_snippets(subject).await;

        let all_snippets: Vec<ContextSnippet> =
            intent_snippets.into_iter().chain(gap_snippets).collect();

        if all_snippets.is_empty() {
            return Ok(Self::empty_section());
        }

        // Use the intent heading when intent snippets are present (primary signal);
        // fall back to the gaps heading so the section is clearly labelled.
        let heading = if has_intent {
            SECTION_HEADING.to_string()
        } else {
            GAPS_HEADING.to_string()
        };

        Ok(ContextSection {
            heading,
            snippets: all_snippets,
        })
    }
}

#[cfg(test)]
#[path = "conformance_tests.rs"]
mod tests;
