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

/// PR number used by the test subject; lets `query_carries_pr_number` assert the
/// real number is threaded into the ISR query rather than the old hard-coded `0`.
const TEST_PR_NUMBER: u64 = 1359;

/// Build a subject whose PR body links a ticket via `Closes #N`.
fn subject_with_body(body: &str) -> ReviewSubject {
    ReviewSubject {
        owner: "bobmatnyc".to_string(),
        repo: "trusty-tools".to_string(),
        title: "Add pagination".to_string(),
        body: body.to_string(),
        changed_files: vec!["src/page.rs".to_string()],
        identifiers: vec![],
        pr_number: TEST_PR_NUMBER,
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

// ─── would_flag predicate (cross-gate AC-18 contribution) ─────────────────────

/// A resolved intent whose TICKET prescribes `m` (precedence: Ticket) — the M5
/// shape (a prescribed method the diff could contradict).
fn ticket_intent(m: &str) -> ResolvedIntent {
    ResolvedIntent {
        ticket: Some(TicketRef {
            id: "#1362".to_string(),
            title: "t".to_string(),
            url: None,
            backend: "github".to_string(),
        }),
        ticket_method: Some(Method {
            text: m.to_string(),
            kind: MethodKind::Approach,
            source_excerpt: m.to_string(),
        }),
        spec_section: None,
        spec_method: None,
        precedence_winner: Precedence::Ticket,
        conflict: false,
        stale_spec: false,
        unresolved: None,
    }
}

/// `would_flag` is TRUE when a prescribed method exists (M1/M2/M5) — the back
/// gate surfaces a method to check the diff against.
///
/// Why: AC-18 asserts FRONT `Escalate` ⇔ BACK would-flag for M5 inputs; this
/// pins the BACK half of that equivalence at the renderer level (spec §5.2).
/// What: a ticket-prescribed-method intent → `would_flag == true`.
/// Test: this test; pure, no network.
#[test]
fn would_flag_true_for_prescribed_method() {
    let intent = ticket_intent("use cursor-based pagination");
    assert!(
        ConformanceSource::would_flag(&intent),
        "a prescribed method must be surfaced (M5 would-flag)"
    );
}

/// `would_flag` is FALSE for a gap (M3) — nothing to flag against.
///
/// Why: AC-18 asserts FRONT `AutoAccept` ⇔ BACK no-finding for M3 inputs; this
/// pins the BACK half (spec §4.1 M3, §4.2).
/// What: `ResolvedIntent::none()` → `would_flag == false`.
/// Test: this test; pure.
#[test]
fn would_flag_false_for_gap() {
    assert!(
        !ConformanceSource::would_flag(&ResolvedIntent::none()),
        "a gap (M3) surfaces no method → no finding possible"
    );
}

/// `would_flag` is FALSE for an `unresolved` (fail-open) intent (AC-11).
///
/// Why: a missing/unfetchable intent source must never manufacture a finding.
/// What: `ResolvedIntent::unresolved(..)` → `would_flag == false`.
/// Test: this test; pure.
#[test]
fn would_flag_false_for_unresolved() {
    assert!(
        !ConformanceSource::would_flag(&ResolvedIntent::unresolved("fetch failed")),
        "an unresolved intent is fail-open → no finding (AC-11)"
    );
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

// ─── build_query: PR-number threading (#1359) ─────────────────────────────────

/// `build_query` threads the real PR number into the ISR query (no hard-coded 0).
///
/// Why: the ISR's `IntentQuery::Pr` keys ticket linkage off the PR (body +
/// number); the source previously hard-coded `pr_number: 0`, losing the real
/// number.  This pins the threading so a regression to `0` is caught.
/// What: builds a query from a subject carrying `TEST_PR_NUMBER` and asserts the
/// resulting `IntentQuery::Pr.pr_number` matches.
/// Test: this test; no network.
#[test]
fn query_carries_pr_number() {
    let subject = subject_with_body("Closes #1325");
    let query = ConformanceSource::build_query(&subject).expect("owner/repo present → Some");
    match query {
        IntentQuery::Pr { pr_number, .. } => {
            assert_eq!(
                pr_number, TEST_PR_NUMBER,
                "the real PR number must be threaded, not hard-coded 0"
            );
        }
        other => panic!("build_query must produce IntentQuery::Pr, got {other:?}"),
    }
}

/// `build_query` returns `None` when there is no owner/repo (local-diff mode).
///
/// Why: a local diff has no PR to resolve intent against; `gather` must skip with
/// an empty section rather than issue a meaningless ISR query.
/// What: a subject with empty owner/repo → `build_query` returns `None`.
/// Test: this test; no network.
#[test]
fn query_none_without_owner_repo() {
    let subject = ReviewSubject {
        owner: String::new(),
        repo: String::new(),
        ..subject_with_body("Closes #1325")
    };
    assert!(
        ConformanceSource::build_query(&subject).is_none(),
        "no owner/repo (local-diff) must yield None"
    );
}

// ─── source_citation: snippet title/subtitle carry the ticket key (#1419) ────

/// The rendered snippet title contains the ticket ID so the LLM can copy it
/// verbatim into `source_citation` (#1419).
///
/// Why: the source-citation grounding mechanism (AC #1419) requires the LLM
/// to have the EXACT ticket key available as a copyable string in the context
/// snippet; if the key is absent from the rendered snippet, the LLM cannot
/// populate `source_citation` reliably.
/// What: renders a `ResolvedIntent` with a known ticket ID and asserts both
/// the snippet title and the subtitle contain that ID.
/// Test: this test; pure, no network.
#[test]
fn render_section_title_and_subtitle_contain_ticket_key() {
    let ticket_id = "IMPL-2026-05-009".to_string();
    let intent = ResolvedIntent {
        ticket: Some(TicketRef {
            id: ticket_id.clone(),
            title: "Paginate listing".to_string(),
            url: None,
            backend: "github".to_string(),
        }),
        ticket_method: Some(Method {
            text: "use cursor-based pagination".to_string(),
            kind: MethodKind::Approach,
            source_excerpt: "use cursor-based pagination".to_string(),
        }),
        spec_section: None,
        spec_method: None,
        precedence_winner: Precedence::Ticket,
        conflict: false,
        stale_spec: false,
        unresolved: None,
    };
    let section = ConformanceSource::render_section(&intent);
    assert!(
        !section.snippets.is_empty(),
        "a prescribed method must produce a non-empty section"
    );
    let snippet = &section.snippets[0];
    assert!(
        snippet.title.contains(&ticket_id),
        "snippet title must contain the ticket ID for source_citation grounding: got {:?}",
        snippet.title
    );
    let subtitle = snippet
        .subtitle
        .as_deref()
        .expect("subtitle must be present");
    assert!(
        subtitle.contains(&ticket_id),
        "snippet subtitle must contain the ticket ID as the citation key: got {:?}",
        subtitle
    );
}

// ─── #1418: test-plan / AC conformance gap tests ──────────────────────────────

/// PR body with a `## Test plan` section yields the contained bullet items.
///
/// Why: `extract_test_plan_items` must correctly identify the `## Test plan`
/// heading (case-insensitive) and collect every bullet line beneath it, stopping
/// at the next `##` heading (#1418 AC-1).
/// What: a body with a `## Test plan` section containing two bullets → both
/// items returned; a non-test-plan heading following them is not included.
/// Test: this test; pure, no network.
#[test]
fn test_plan_extraction_from_pr_body() {
    let body = "\
## Summary\n\
This PR adds pagination.\n\
\n\
## Test plan\n\
- Verify the endpoint returns 20 items per page\n\
- Should fail gracefully on invalid cursor\n\
\n\
## Related\n\
- Some other note\n\
";
    let items = super::extract_test_plan_items(body);
    assert_eq!(items.len(), 2, "two bullet items under ## Test plan");
    assert!(
        items[0].contains("Verify the endpoint"),
        "first item text: {:?}",
        items[0]
    );
    assert!(
        items[1].contains("Should fail gracefully"),
        "second item text: {:?}",
        items[1]
    );
}

/// Ticket body with `## Acceptance Criteria` bullets and `- [ ]` checkboxes
/// yields all AC items, deduped (#1418 AC-2).
///
/// Why: tickets use both heading-scoped bullets and freestanding `- [ ]`
/// checklist items; `extract_ac_bullets` must collect both forms.
/// What: a ticket body with an `## Acceptance Criteria` section (plain bullet)
/// and a `- [ ]` checkbox item elsewhere → both are returned, no duplicates.
/// Test: this test; pure, no network.
#[test]
fn ac_extraction_from_ticket() {
    let ticket_body = "\
## Background\n\
Some background text.\n\
\n\
## Acceptance Criteria\n\
- Tests cover the cursor path\n\
- Error handling is documented\n\
\n\
## Implementation notes\n\
- [ ] Should validate the cursor token\n\
";
    let items = super::extract_ac_bullets(ticket_body);
    assert_eq!(
        items.len(),
        3,
        "expected 3 AC items (2 from section + 1 from - [ ] checkbox): {:?}",
        items
    );
    assert!(
        items.iter().any(|i| i.contains("cursor")),
        "cursor-related item must be present: {:?}",
        items
    );
    assert!(
        items.iter().any(|i| i.contains("validate")),
        "validate item from - [ ] must be present: {:?}",
        items
    );
}

/// An AC item referencing test behaviour with no test file in the diff produces
/// an "Unmet AC" snippet with a source citation subtitle (#1418 AC-3).
///
/// Why: the primary payoff of #1418 is a citable gap finding the LLM can
/// reference instead of emitting a vague "needs tests" note.  The snippet title
/// must start with "Unmet AC:" and the subtitle must carry the source.
/// What: `build_gap_snippets` with one test-related AC item and no test-file
/// coverage → one snippet with the expected title/subtitle.
/// Test: this test; pure (calls `build_gap_snippets` directly).
#[test]
fn unmet_ac_renders_snippet() {
    let ac_items = vec!["Should verify the pagination cursor is valid".to_string()];
    let changed_files = vec!["src/page.rs".to_string()]; // no test file
    let identifiers: Vec<String> = Vec::new();

    let snippets = super::build_gap_snippets(
        &[],
        &ac_items,
        Some("https://github.com/owner/repo/issues/42"),
        Some("#42"),
        &changed_files,
        &identifiers,
    );
    assert_eq!(
        snippets.len(),
        1,
        "one unmet AC item must produce one snippet"
    );
    let snip = &snippets[0];
    assert!(
        snip.title.starts_with("Unmet AC:"),
        "snippet title must start with 'Unmet AC:': got {:?}",
        snip.title
    );
    let subtitle = snip.subtitle.as_deref().expect("subtitle must be present");
    assert!(
        subtitle.contains("#42"),
        "subtitle must carry ticket ID: got {:?}",
        subtitle
    );
}

/// A PR that touches an UNRELATED test file does NOT count as covering an AC
/// item whose key terms do not appear in any test file path or test identifier.
///
/// Why: `item_has_test_coverage` must be PER-ITEM — touching `tests/auth_test.rs`
/// must not suppress a gap finding for "verify pagination cursor is valid"; the
/// two concerns are unrelated (#1418 review fix, Issue 1).
/// What: a changed_files list containing a test file for "auth" and an AC item
/// about "pagination cursor" → gap snippet IS emitted (the auth test does not
/// cover the pagination AC item).
/// Test: this test; pure (calls `build_gap_snippets` directly).
#[test]
fn unrelated_test_file_does_not_cover_ac_item() {
    let ac_items = vec!["verify pagination cursor is valid".to_string()];
    // An auth test file — unrelated to pagination.
    let changed_files = vec!["tests/auth_test.rs".to_string()];
    let identifiers: Vec<String> = Vec::new();

    let snippets = super::build_gap_snippets(
        &[],
        &ac_items,
        Some("https://github.com/owner/repo/issues/55"),
        Some("#55"),
        &changed_files,
        &identifiers,
    );
    assert_eq!(
        snippets.len(),
        1,
        "unrelated test file must NOT count as coverage; gap snippet expected: {:?}",
        snippets
    );
    assert!(
        snippets[0].title.contains("pagination"),
        "gap snippet must reference the AC item: {:?}",
        snippets[0].title
    );
}

/// A PR that touches a test file whose path shares key terms with the AC item
/// IS considered to cover that item (the permissive heuristic).
///
/// Why: if the test file path contains a key term from the AC item, we trust
/// the author added coverage for it; we must not emit a false-positive gap.
/// What: `changed_files` with `tests/pagination_test.rs` and an AC item about
/// "pagination cursor" → no gap snippet emitted.
/// Test: this test; pure.
#[test]
fn related_test_file_covers_ac_item() {
    let ac_items = vec!["verify pagination cursor is valid".to_string()];
    let changed_files = vec!["tests/pagination_test.rs".to_string()];
    let identifiers: Vec<String> = Vec::new();

    let snippets = super::build_gap_snippets(
        &[],
        &ac_items,
        Some("https://github.com/owner/repo/issues/55"),
        Some("#55"),
        &changed_files,
        &identifiers,
    );
    assert!(
        snippets.is_empty(),
        "a test file sharing key terms with the AC item must be treated as covering it: {:?}",
        snippets
    );
}

/// `build_gap_snippets` returns empty when both lists are empty (no advisory).
///
/// Why: the advisory branch was removed because `gather_gap_snippets` already
/// gates on having items to check before calling this function; emitting an
/// advisory from `build_gap_snippets` would be dead code in production and
/// confusing to direct callers.
/// What: empty `tp_items` + empty `ac_items` → empty Vec.
/// Test: this test; pure.
#[test]
fn empty_lists_produce_no_snippets() {
    let snippets =
        super::build_gap_snippets(&[], &[], None, None, &["src/foo.rs".to_string()], &[]);
    assert!(
        snippets.is_empty(),
        "empty tp_items + empty ac_items must produce no snippets (advisory removed): {:?}",
        snippets
    );
}

/// A completely empty PR body produces no snippets at all (not even advisory).
///
/// Why: an empty PR body signals a local-diff / draft review where there is no
/// authored test plan to check against; emitting an advisory would be a false
/// positive (the author hasn't described the PR yet).
/// What: a subject with `body = ""` → `gather_gap_snippets` returns an empty Vec
/// (checked by asserting `gather` returns an empty section when the ISR also
/// resolves to no intent).
/// Test: this test; no network.
#[tokio::test]
async fn empty_body_no_snippets() {
    let src = source_with_fetcher(MockFetcher {
        body: String::new(),
        fail: false,
    });
    let subject = ReviewSubject {
        body: String::new(), // empty body
        ..subject_with_body("")
    };
    let section = src.gather(&subject).await.expect("gather must not error");
    assert!(
        section.snippets.is_empty(),
        "empty PR body must produce no snippets (not even advisory)"
    );
}

/// `extract_test_plan_items` accepts tab-separated bullets (e.g. `-\titem`) as
/// well as space-separated bullets, consistent with the checkbox branch in
/// `extract_ac_bullets` (#1418 fix 4).
///
/// Why: some editors / PR templates emit tab-after-marker bullets; `parse_bullet`
/// must accept them or silently drop items from the test-plan parse.
/// What: a PR body with `"-\tVerify the tab bullet"` under `## Test plan` →
/// the item is returned (not dropped).
/// Test: this test; pure, no network.
#[test]
fn test_plan_accepts_tab_separated_bullets() {
    let body = "## Test plan\n-\tVerify the tab bullet is parsed\n";
    let items = super::extract_test_plan_items(body);
    assert_eq!(
        items.len(),
        1,
        "tab-separated bullet must be parsed: {:?}",
        items
    );
    assert!(
        items[0].contains("tab bullet"),
        "item text must survive tab stripping: {:?}",
        items[0]
    );
}
