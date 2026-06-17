//! AC-18 cross-gate golden test (DOC-15 C5, #1362) — the capstone of the
//! intent/method-conformance epic (#1354).
//!
//! Why: the whole point of the shared intent-source resolver (ISR) is that the
//! FRONT gate (trusty-mpm, pre-work) and the BACK gate (trusty-review,
//! post-implementation) can **never disagree** about a given intent (spec §6.5,
//! goal G1/G4). AC-18 (spec §8) makes that guarantee a *test*: the SAME
//! `ResolvedIntent` must drive both gates to consistent matrix outcomes — FRONT
//! `Escalate` ⇔ BACK would-flag for **M5** inputs (divergence), and FRONT
//! `AutoAccept` ⇔ BACK no-finding for **M3** inputs (gap). A regression in either
//! gate's matrix mapping (e.g. the FRONT gate stops escalating on divergence, or
//! the BACK renderer starts surfacing a method for a gap) breaks this equivalence
//! and fails here — which is exactly the cross-gate contract the epic promised.
//!
//! What: this is a genuine cross-crate test. It lives in trusty-mpm (which
//! dev-depends on trusty-review, see Cargo.toml) so it can call BOTH real gate
//! entry points against ONE shared intent value — the FRONT entry point
//! `trusty_mpm::daemon::managed_routes::front_gate::front_gate_disposition` (the
//! pure matrix→`Disposition` mapping, spec §4.1 / §5.1), and the BACK entry point
//! `trusty_review::integrations::context::ConformanceSource::would_flag` (derived
//! from the real `render_section` renderer, spec §5.2).
//! The shared `ResolvedIntent` is produced by the REAL ISR
//! (`trusty_common::intent_source::resolve_default`) from a fixture ticket body,
//! using offline mock `TicketFetcher`/`SpecLookup` seams — so the test exercises
//! the actual resolution + precedence path, not a hand-built struct, and never
//! touches the network.
//!
//! Test: this file IS the AC-18 golden test. Run with
//! `cargo test -p trusty-mpm --test conformance_cross_gate`.

use async_trait::async_trait;

use trusty_common::intent_source::{
    ChangedFile, IntentQuery, IsrError, Method, MethodKind, Precedence, ResolvedIntent, SpecLookup,
    TicketData, TicketFetcher, heuristic_method, resolve_default,
};

use trusty_mpm::daemon::managed_routes::front_gate::front_gate_disposition;
use trusty_review::integrations::context::ConformanceSource;

// ───────────────────────── offline ISR seams ─────────────────────────────────

/// A `TicketFetcher` returning a fixed ticket body (no network).
///
/// Why: the golden test must drive the REAL resolver offline; this injects the
/// ticket body that determines whether the resolved intent is an M5 (a
/// prescribed method) or an M3 (a gap).
/// What: returns a `TicketData` carrying `body` for any id.
/// Test: used by `resolve_fixture` below.
struct FixtureFetcher {
    body: String,
}

#[async_trait]
impl TicketFetcher for FixtureFetcher {
    async fn fetch(
        &self,
        _owner: &str,
        _repo: &str,
        ticket_id: &str,
    ) -> Result<TicketData, IsrError> {
        Ok(TicketData {
            id: ticket_id.to_string(),
            title: "AC-18 fixture".to_string(),
            body: self.body.clone(),
            url: Some("https://github.com/x/y/issues/1362".to_string()),
            backend: "github".to_string(),
        })
    }
}

/// A `SpecLookup` that resolves no spec (the spec axis is intentionally a gap so
/// the fixture's intent is driven purely by the ticket body).
struct NoSpecLookup;
impl SpecLookup for NoSpecLookup {
    fn load(&self, _spec_file: &str) -> Option<String> {
        None
    }
}

/// Resolve a shared `ResolvedIntent` from a ticket `body`, via the REAL ISR.
///
/// Why: AC-18 demands the SAME resolved intent drive both gates; producing it
/// through `resolve_default` (the actual resolver + precedence) — rather than
/// hand-constructing the struct — proves the gates agree on what the resolver
/// *actually* returns (spec §6.1).
/// What: builds a `Ticket` query with no changed files (spec axis = gap), runs
/// `resolve_default` with the offline fetcher + `NoSpecLookup`, and returns the
/// resolved intent.
/// Test: used by both `ac18_*` cases below.
async fn resolve_fixture(body: &str) -> ResolvedIntent {
    let fetcher = FixtureFetcher {
        body: body.to_string(),
    };
    let query = IntentQuery::Ticket {
        ticket_id: "#1362".to_string(),
        owner: "bobmatnyc".to_string(),
        repo: "trusty-tools".to_string(),
        changed_files: Vec::<ChangedFile>::new(),
    };
    resolve_default(query, &fetcher, &NoSpecLookup).await
}

/// A planned method for the FRONT gate (the pre-work "subject" being judged).
fn planned(text: &str) -> Method {
    Method {
        text: text.to_string(),
        kind: MethodKind::Approach,
        source_excerpt: text.to_string(),
    }
}

// ═════════════════════ AC-18: the cross-gate equivalence ═════════════════════

/// AC-18 (M5): FRONT `Escalate` ⇔ BACK would-flag for a divergent method.
///
/// Why: M5 is the *divergence* row (spec §4.1) — the subject contradicts an
/// explicit prescribed method. The cross-gate contract requires the FRONT gate to
/// escalate (before coding) and the BACK gate to be able to flag (during review)
/// for the SAME resolved intent. If they ever diverge here, the epic's central
/// guarantee is broken.
/// What: resolves a fixture whose ticket prescribes "use cursor-based pagination"
/// (→ `precedence_winner = Ticket`, a prescribed method), then:
///   - drives the REAL FRONT gate with a *diverging* planned method
///     ("use offset-based pagination") and asserts `Escalate`;
///   - drives the REAL BACK gate (`would_flag`) on the SAME intent and asserts it
///     would flag (true);
///   - asserts the two outcomes are EQUIVALENT (escalate ⇔ would-flag).
/// Test: this test; offline (mock ISR seams), no network.
#[tokio::test]
async fn ac18_m5_divergence_front_escalate_iff_back_would_flag() {
    // One shared intent, resolved by the real ISR. The ticket prescribes a
    // method → M-class intent (M1; a divergent subject makes it M5).
    let intent = resolve_fixture(
        "Implement the listing endpoint. We must use cursor-based pagination, not offset.",
    )
    .await;

    // Sanity: the fixture really is a prescribed-method (non-gap) intent.
    assert_eq!(
        intent.precedence_winner,
        Precedence::Ticket,
        "M5 fixture must resolve a ticket-prescribed method"
    );
    assert!(
        intent.ticket_method.is_some(),
        "M5 fixture must carry a ticket method"
    );
    assert!(!intent.is_gap(), "M5 fixture must NOT be a gap");

    // FRONT gate (real): a planned method that DIVERGES from the prescribed one.
    let diverging_plan = planned("use offset-based pagination");
    let front = front_gate_disposition(&intent, Some(&diverging_plan));
    let front_escalates = !front.is_auto_accept();

    // BACK gate (real): would the conformance source surface a method to flag?
    let back_would_flag = ConformanceSource::would_flag(&intent);

    assert!(front_escalates, "M5: FRONT must escalate on divergence");
    assert!(back_would_flag, "M5: BACK must be able to flag (M5)");
    // The AC-18 equivalence itself: escalate ⇔ would-flag.
    assert_eq!(
        front_escalates, back_would_flag,
        "AC-18 (M5): FRONT escalate must hold IFF BACK would-flag"
    );
}

/// AC-18 (M3): FRONT `AutoAccept` ⇔ BACK no-finding for a gap.
///
/// Why: M3 is the *gap* row (spec §4.1) — neither ticket nor spec prescribes a
/// method, so there is nothing to conform to. The cross-gate contract requires
/// the FRONT gate to auto-proceed and the BACK gate to emit nothing for the SAME
/// resolved intent.
/// What: resolves a fixture whose ticket body has no imperative-method cue
/// (→ a gap, `precedence_winner = None`), then:
///   - drives the REAL FRONT gate with no planned method and asserts `AutoAccept`;
///   - drives the REAL BACK gate (`would_flag`) on the SAME intent and asserts it
///     would NOT flag (false);
///   - asserts the two outcomes are EQUIVALENT (auto-accept ⇔ no-finding).
/// Test: this test; offline, no network.
#[tokio::test]
async fn ac18_m3_gap_front_auto_accept_iff_back_no_finding() {
    // One shared intent, resolved by the real ISR. The body prescribes NO method
    // → a gap (M3).
    let intent =
        resolve_fixture("Add a new field to the response payload. See the discussion thread.")
            .await;

    // Sanity: the fixture really is a gap.
    assert!(intent.is_gap(), "M3 fixture must be a gap");
    assert_eq!(
        intent.precedence_winner,
        Precedence::None,
        "M3 fixture must have no precedence winner"
    );

    // FRONT gate (real): no planned method (pre-work plan is implicit) — and even
    // a gap intent has nothing to conform to.
    let front = front_gate_disposition(&intent, None);
    let front_auto_accepts = front.is_auto_accept();

    // BACK gate (real): a gap surfaces no method → no finding possible.
    let back_no_finding = !ConformanceSource::would_flag(&intent);

    assert!(front_auto_accepts, "M3: FRONT must auto-accept on a gap");
    assert!(back_no_finding, "M3: BACK must emit no finding on a gap");
    // The AC-18 equivalence itself: auto-accept ⇔ no-finding.
    assert_eq!(
        front_auto_accepts, back_no_finding,
        "AC-18 (M3): FRONT auto-accept must hold IFF BACK emits no finding"
    );
}

/// AC-18 (cross-check): the heuristic that derives the FRONT planned method is
/// the SAME one the ISR uses for the ticket method — so "divergence" is measured
/// on a like-for-like basis, not an artefact of two different extractors.
///
/// Why: the FRONT gate derives its planned method from the task prose via
/// `heuristic_method` (front_gate.rs), and the ISR derives the ticket method via
/// the same heuristic. Pinning that they classify the divergent/prescribed pair
/// consistently guards against a subtle false-equivalence where the gates "agree"
/// only because one extractor silently dropped a method.
/// What: asserts the prescribed cue extracts an Approach method and the divergent
/// plan extracts a different Approach method (so the FRONT comparison is genuine).
/// Test: this test; pure.
#[test]
fn ac18_planned_and_prescribed_extract_consistently() {
    let prescribed = heuristic_method("use cursor-based pagination").expect("prescribed method");
    let divergent = heuristic_method("use offset-based pagination").expect("divergent method");
    assert_eq!(prescribed.kind, MethodKind::Approach);
    assert_eq!(divergent.kind, MethodKind::Approach);
    assert_ne!(
        prescribed.text.to_lowercase(),
        divergent.text.to_lowercase(),
        "the prescribed and divergent methods must differ (genuine M5 divergence)"
    );
}
