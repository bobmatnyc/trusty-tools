//! Tests for the intent/method-conformance back-gate context source (#1359).
//!
//! Why: extracted to a sibling file to keep `conformance.rs` under the 500-line
//! cap while covering the AC-8..AC-12 back-gate behaviours that live in the
//! source (the verdict-floor cap is tested in `grade_tests.rs`; the
//! finding-category parse in `parser_tests.rs`).
//! What: drives `gather` against MOCK ISR seams (`TicketFetcher` / `SpecLookup`)
//! so no network or GitHub auth is touched — a ticket-method contradiction
//! renders a section; an unresolved/gap intent fails open to an empty section;
//! semantic mode errors; a disabled source skips.
//! Test: this file is the test module included from `conformance.rs`.

use super::*;
use trusty_common::intent_source::{
    Method, MethodKind, Precedence, ResolvedIntent, TicketData, TicketRef,
};

// ─── Mock ISR seams (no network) ──────────────────────────────────────────────

/// A `TicketFetcher` that returns a fixed body (or a failure) for any id.
struct MockFetcher {
    body: String,
    fail: bool,
}

#[async_trait]
impl TicketFetcher for MockFetcher {
    async fn fetch(
        &self,
        _owner: &str,
        _repo: &str,
        ticket_id: &str,
    ) -> Result<TicketData, IsrError> {
        if self.fail {
            return Err(IsrError::TicketFetch("mock fetch failure".to_string()));
        }
        Ok(TicketData {
            id: ticket_id.to_string(),
            title: "Mock ticket".to_string(),
            body: self.body.clone(),
            url: Some("https://example/issues/1325".to_string()),
            backend: "github".to_string(),
        })
    }
}

/// A `SpecLookup` that never resolves a spec (spec axis is a gap in these tests).
struct NoSpecLookup;
impl SpecLookup for NoSpecLookup {
    fn load(&self, _spec_file: &str) -> Option<String> {
        None
    }
}

/// Build a subject whose PR body links a ticket via `Closes #N`.
fn subject_with_body(body: &str) -> ReviewSubject {
    ReviewSubject {
        owner: "bobmatnyc".to_string(),
        repo: "trusty-tools".to_string(),
        title: "Add pagination".to_string(),
        body: body.to_string(),
        changed_files: vec!["src/page.rs".to_string()],
        identifiers: vec![],
    }
}

fn source_with_fetcher(fetcher: MockFetcher) -> ConformanceSource {
    ConformanceSource::new(
        true,
        RetrievalMode::Live,
        Box::new(fetcher),
        Box::new(NoSpecLookup),
    )
}

// ─── gather: happy path (ticket method rendered) ──────────────────────────────

/// A ticket whose body prescribes a method renders a non-empty section naming
/// the prescribed method (the thing to check conformance against).
///
/// Why: the back gate must surface the resolved ticket method to the reviewer
/// LLM so it can flag a contradicting diff (M1).  AC-8's finding emission is the
/// LLM's job; the source's job is to render the intent.
/// What: a `Closes #1325` body + a ticket body prescribing "use cursor-based
/// pagination" → a section with a snippet whose body carries the method text.
/// Test: this test; no network.
#[tokio::test]
async fn gather_renders_ticket_method() {
    let fetcher = MockFetcher {
        body: "Implement listing. Method: use cursor-based pagination, not offset.".to_string(),
        fail: false,
    };
    let src = source_with_fetcher(fetcher);
    let subject = subject_with_body("Closes #1325 — add the listing endpoint.");
    let section = src.gather(&subject).await.expect("gather must not error");
    assert_eq!(section.heading, "Intended method (ticket/spec)");
    assert!(
        !section.snippets.is_empty(),
        "a ticket with a prescribed method must render a non-empty section"
    );
    let rendered = section
        .snippets
        .iter()
        .filter_map(|s| s.body.clone())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        rendered.to_lowercase().contains("cursor"),
        "the prescribed method text must be surfaced: {rendered}"
    );
}

// ─── gather: fail-open (AC-11) ────────────────────────────────────────────────

/// A ticket-fetch failure (ISR unresolved) yields an EMPTY section and NO
/// conformance content (AC-11 fail-open).
///
/// Why: a missing/unfetchable intent source must never manufacture a finding;
/// the source returns an empty section the orchestrator drops.
/// What: a failing `MockFetcher` → empty section, gather returns Ok.
/// Test: this test; no network.
#[tokio::test]
async fn gather_fail_open_on_unresolved() {
    let src = source_with_fetcher(MockFetcher {
        body: String::new(),
        fail: true,
    });
    let subject = subject_with_body("Closes #1325");
    let section = src.gather(&subject).await.expect("fail-open: Ok, not Err");
    assert!(
        section.snippets.is_empty(),
        "an unresolved ISR must render an EMPTY section (AC-11 fail-open)"
    );
}

/// A PR with NO ticket linkage resolves to no intent → empty section (AC-11).
///
/// Why: non-ticketed work has no intent to conform to; the gate no-ops.
/// What: a body with no `Closes #N` → ISR `none()` → empty section.
/// Test: this test; no network.
#[tokio::test]
async fn gather_no_linkage_renders_empty() {
    let src = source_with_fetcher(MockFetcher {
        body: "use cursor pagination".to_string(),
        fail: false,
    });
    let subject = subject_with_body("A PR with no ticket reference at all.");
    let section = src.gather(&subject).await.expect("Ok");
    assert!(
        section.snippets.is_empty(),
        "no ticket linkage → empty section (no intent to conform to)"
    );
}

/// A ticket with NO prescribed method (a gap, M3) renders an empty section.
///
/// Why: a gap is advisory/none — never a blocking finding (AC-9 / M3); the
/// source surfaces nothing to flag against.
/// What: a fetched ticket whose body prescribes no method → empty section.
/// Test: this test; no network.
#[tokio::test]
async fn gather_gap_renders_empty() {
    let src = source_with_fetcher(MockFetcher {
        body: "Please add a feature flag. Thanks!".to_string(),
        fail: false,
    });
    let subject = subject_with_body("Closes #1325");
    let section = src.gather(&subject).await.expect("Ok");
    assert!(
        section.snippets.is_empty(),
        "a ticket with no prescribed method is a gap (M3) → empty section (AC-9)"
    );
}

// ─── render_section: stale-spec advisory (M4) ─────────────────────────────────

/// A stale-spec conflict (M4) renders the ticket method PLUS an advisory snippet
/// for the conflicting spec — never as the thing to fail against.
///
/// Why: under precedence the ticket wins; the conflicting spec is downgraded to
/// advisory context (spec §5.2 precedence wiring, M4 → ADVISORY).
/// What: a `ResolvedIntent` with ticket+spec methods, `stale_spec = true`, and
/// `precedence_winner = Ticket` → two snippets, one flagged advisory/stale.
/// Test: this test; constructs the intent directly (renderer is pure).
#[test]
fn render_stale_spec_advisory() {
    let intent = ResolvedIntent {
        ticket: Some(TicketRef {
            id: "#1325".to_string(),
            title: "t".to_string(),
            url: None,
            backend: "github".to_string(),
        }),
        ticket_method: Some(Method {
            text: "add dependency X".to_string(),
            kind: MethodKind::Approach,
            source_excerpt: "add dependency X".to_string(),
        }),
        spec_section: None,
        spec_method: Some(Method {
            text: "no new dependencies".to_string(),
            kind: MethodKind::Constraint,
            source_excerpt: "no new dependencies".to_string(),
        }),
        precedence_winner: Precedence::Ticket,
        conflict: true,
        stale_spec: true,
        unresolved: None,
    };
    let section = ConformanceSource::render_section(&intent);
    assert_eq!(
        section.snippets.len(),
        2,
        "ticket method + stale-spec advisory"
    );
    let advisory = section
        .snippets
        .iter()
        .any(|s| s.title.to_lowercase().contains("stale"));
    assert!(
        advisory,
        "the conflicting spec must be rendered as a stale advisory (M4)"
    );
}

/// An `unresolved` intent renders an empty section (renderer-level AC-11).
///
/// Why: pin the renderer's fail-open behaviour independently of `gather`.
/// What: `ResolvedIntent::unresolved(..)` → empty section.
/// Test: this test; pure.
#[test]
fn render_unresolved_is_empty() {
    let intent = ResolvedIntent::unresolved("ticket fetch failed");
    let section = ConformanceSource::render_section(&intent);
    assert!(section.snippets.is_empty());
}

// ─── gather: mode + enabled ───────────────────────────────────────────────────

/// Semantic mode is not implemented (PR-B parity) → error (logged, fail-open by
/// the orchestrator).
///
/// Why: like every live source, conformance only supports `Live` retrieval in
/// C2; a `Semantic` config surfaces a clear not-implemented error.
/// What: a `Semantic`-mode source → `SemanticNotImplemented`.
/// Test: this test; no network.
#[tokio::test]
async fn semantic_mode_errors() {
    let src = ConformanceSource::new(
        true,
        RetrievalMode::Semantic,
        Box::new(MockFetcher {
            body: String::new(),
            fail: false,
        }),
        Box::new(NoSpecLookup),
    );
    let subject = subject_with_body("Closes #1325");
    let err = src.gather(&subject).await.unwrap_err();
    assert!(matches!(
        err,
        ContextSourceError::SemanticNotImplemented { .. }
    ));
}

/// A subject with no owner/repo (local-diff mode) renders an empty section.
///
/// Why: no repo scope → nothing to resolve; skip with an empty section, not an
/// error.
/// What: an empty-owner subject → empty section.
/// Test: this test; no network.
#[tokio::test]
async fn gather_local_diff_renders_empty() {
    let src = source_with_fetcher(MockFetcher {
        body: "use cursor pagination".to_string(),
        fail: false,
    });
    let subject = ReviewSubject::default(); // empty owner/repo
    let section = src.gather(&subject).await.expect("Ok");
    assert!(section.snippets.is_empty());
}

/// `from_config` honours an explicit `enabled = false` (default-disabled source).
///
/// Why: the conformance source is opt-in (it needs GitHub auth); an explicit
/// disable must keep it off, and the default (no creds auto-enable) is off.
/// What: a `SourceConfig { enabled: Some(false), .. }` → `is_enabled() == false`;
/// a default `SourceConfig` → also `false` (no auto-enable on cred presence).
/// Test: this test; no network.
#[test]
fn from_config_respects_explicit_disable() {
    let cfg_off = crate::integrations::context::SourceConfig {
        enabled: Some(false),
        mode: RetrievalMode::Live,
    };
    let src = ConformanceSource::from_config(&cfg_off, RunMode::Cli, ReviewConfig::load(None));
    assert!(
        !src.is_enabled(),
        "explicit disable must keep the source off"
    );

    let cfg_default = crate::integrations::context::SourceConfig::default();
    let src2 = ConformanceSource::from_config(&cfg_default, RunMode::Cli, ReviewConfig::load(None));
    assert!(
        !src2.is_enabled(),
        "default conformance source is DISABLED (no auto-enable)"
    );
}

/// `from_config` honours an explicit `enabled = true`.
///
/// Why: an operator opting in must turn the source on.
/// What: `SourceConfig { enabled: Some(true), .. }` → `is_enabled() == true`.
/// Test: this test; no network.
#[test]
fn from_config_respects_explicit_enable() {
    let cfg_on = crate::integrations::context::SourceConfig {
        enabled: Some(true),
        mode: RetrievalMode::Live,
    };
    let src = ConformanceSource::from_config(&cfg_on, RunMode::Cli, ReviewConfig::load(None));
    assert!(src.is_enabled(), "explicit enable must turn the source on");
    assert_eq!(src.name(), "conformance");
}
