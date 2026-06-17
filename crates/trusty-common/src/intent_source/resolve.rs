//! The ISR entry point: `resolve(IntentQuery) -> ResolvedIntent`.
//!
//! Why: both conformance gates call one resolver so ticket+spec resolution and
//! the precedence rule (ticket > spec) are implemented **once**, centrally
//! (spec §6.1, §6.5, G4). This module wires the pieces — linkage, ticket fetch,
//! spec resolution, method extraction — and applies precedence exactly once.
//! What: the public async `resolve`, the pluggable `TicketFetcher` /
//! `IntentTokenResolver` seams (§7.4), and `apply_precedence` (the normative
//! four-case rule). Every failure path is **fail-open**: it returns
//! `ResolvedIntent::unresolved{reason}`, never an `Err` (spec §4.2).
//! Test: `super::tests` (AC-1..AC-7).

use async_trait::async_trait;

use super::extract::{HeuristicMethodExtractor, MethodExtractor};
use super::linkage::extract_pr_ticket;
use super::spec_resolve::{extract_spec_method, parse_spec_refs};
use super::types::{
    ChangedFile, IntentQuery, IsrError, Method, Precedence, ResolvedIntent, SpecRef, TicketRef,
};

/// Pluggable token resolver for GitHub App / JWT auth (spec §7.4).
///
/// Why: trusty-review's serve mode uses richer GitHub App/JWT auth than the
/// `tickets` backend's PAT-only path. The ISR must NOT hard-wire a PAT; it
/// exposes this seam (mirroring review's `IssueTokenResolver`) so it works in
/// webhook/serve mode too (spec §7.4).
/// What: one async method returning a bearer token for an owner/repo, or an
/// error when none is available.
/// Test: `super::tests::token_resolver_*`.
#[async_trait]
pub trait IntentTokenResolver: Send + Sync {
    /// Resolve a GitHub access token for `owner`/`repo`.
    ///
    /// Why: the fetch needs a bearer token; how it is obtained (PAT, App JWT)
    /// is the caller's concern (spec §7.4).
    /// What: returns the token string, or `Err` when no token is available.
    /// Test: `super::tests::token_resolver_*`.
    async fn token(&self, owner: &str, repo: &str) -> Result<String, IsrError>;
}

/// Default token resolver: reads the `GITHUB_TOKEN` environment variable.
///
/// Why: the common (CLI / PAT) case needs no custom resolver; this preserves
/// the existing `tickets` backend behaviour as the zero-config default
/// (spec §7.4) while keeping the App/JWT path pluggable.
/// What: returns `$GITHUB_TOKEN`, or `IsrError::NoToken` when it is unset.
/// Test: `super::tests::token_resolver_env_*` (serial, env-mutating).
#[derive(Debug, Clone, Copy, Default)]
pub struct EnvTokenResolver;

#[async_trait]
impl IntentTokenResolver for EnvTokenResolver {
    async fn token(&self, _owner: &str, _repo: &str) -> Result<String, IsrError> {
        std::env::var("GITHUB_TOKEN")
            .map_err(|_| IsrError::NoToken("GITHUB_TOKEN not set".to_string()))
    }
}

/// Pluggable ticket fetcher (the seam over `tickets::Backend::get_issue`).
///
/// Why: the resolver must fetch a ticket body without the production GitHub
/// client in tests, and without coupling `resolve` to one backend. This trait
/// is the seam both the default GitHub fetcher and test mocks implement
/// (spec §6.6 reuses `Backend::get_issue` behind it).
/// What: one async method mapping `(owner, repo, ticket_id)` to a `TicketData`,
/// or `IsrError` on failure.
/// Test: `super::tests` uses a `MockFetcher` implementing this trait.
#[async_trait]
pub trait TicketFetcher: Send + Sync {
    /// Fetch the ticket identified by `ticket_id` in `owner`/`repo`.
    ///
    /// Why: the ticket body is the ticket-method source (spec §6.2).
    /// What: returns the fetched `TicketData`, or `IsrError::TicketFetch`.
    /// Test: `super::tests` (via `MockFetcher`).
    async fn fetch(&self, owner: &str, repo: &str, ticket_id: &str)
    -> Result<TicketData, IsrError>;
}

/// The minimal ticket data the resolver needs (backend-agnostic).
///
/// Why: decouples the resolver from the full `tickets::Issue` shape so a mock
/// (and a future non-GitHub backend) need only supply these fields.
/// What: id, title, body, optional URL, and the backend tag — exactly the
/// inputs to `TicketRef` + method extraction (spec §6.1, §6.2).
/// Test: `super::tests` constructs `TicketData` directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TicketData {
    /// Backend-native id (`#1358`).
    pub id: String,
    /// Human-readable title.
    pub title: String,
    /// Free-prose body — the ticket-method source.
    pub body: String,
    /// Canonical URL, when available.
    pub url: Option<String>,
    /// Backend identifier (`github`).
    pub backend: String,
}

/// Resolve intent from a query, applying precedence (ticket > spec).
///
/// Why: the one entry point both gates call; centralising resolution +
/// precedence guarantees FRONT and BACK can never disagree (spec §6.1, G4).
/// What: extracts the ticket-id (for `Pr`), fetches the ticket via `fetcher`,
/// extracts the ticket method via `extractor`, resolves the spec section + spec
/// method from `changed_files`, then applies [`apply_precedence`]. Any
/// fetch/parse failure degrades to `ResolvedIntent::unresolved` (fail-open,
/// spec §4.2) — this fn never returns `Err`. Non-ticketed `Pr` input →
/// `ResolvedIntent::none()`.
/// Test: `super::tests` (AC-1..AC-7).
pub async fn resolve(
    query: IntentQuery,
    fetcher: &dyn TicketFetcher,
    extractor: &dyn MethodExtractor,
    spec_lookup: &dyn SpecLookup,
) -> ResolvedIntent {
    let (owner, repo, ticket_id, changed_files) = match resolve_ticket_id(&query) {
        Some(t) => t,
        // Non-ticketed PR → clean gap (no intent). Spec §4.2 preconditions.
        None => return ResolvedIntent::none(),
    };

    // ── Ticket axis (fail-open on fetch error) ────────────────────────────
    let ticket_data = match fetcher.fetch(&owner, &repo, &ticket_id).await {
        Ok(t) => t,
        Err(e) => return ResolvedIntent::unresolved(e.to_string()),
    };
    let ticket_method = extractor.extract(&ticket_data.body);
    let ticket_ref = TicketRef {
        id: ticket_data.id,
        title: ticket_data.title,
        url: ticket_data.url,
        backend: ticket_data.backend,
    };

    // ── Spec axis (gap, never an error: ISR never invents linkage) ────────
    let (spec_section, spec_method) = resolve_spec(&changed_files, spec_lookup);

    build_resolved(ticket_ref, ticket_method, spec_section, spec_method)
}

/// Pull `(owner, repo, ticket_id, changed_files)` out of a query.
///
/// Why: `Pr` must derive the ticket-id from linkage while `Ticket` already has
/// it; isolating that branch keeps `resolve` linear (spec §6.1, §6.3).
/// What: for `Ticket`, returns its fields directly; for `Pr`, runs
/// [`extract_pr_ticket`] and returns `None` when the PR has no linkage.
/// Test: `super::tests::linkage_pr_*` (AC-5), `ticket_query_*`.
fn resolve_ticket_id(query: &IntentQuery) -> Option<(String, String, String, Vec<ChangedFile>)> {
    match query {
        IntentQuery::Ticket {
            ticket_id,
            owner,
            repo,
            changed_files,
        } => Some((
            owner.clone(),
            repo.clone(),
            ticket_id.clone(),
            changed_files.clone(),
        )),
        IntentQuery::Pr {
            owner,
            repo,
            body,
            branch,
            commit_messages,
            changed_files,
            ..
        } => extract_pr_ticket(body, commit_messages, branch.as_deref())
            .map(|id| (owner.clone(), repo.clone(), id, changed_files.clone())),
    }
}

/// Resolve the spec section + spec method from changed files.
///
/// Why: the spec axis is greenfield and must never fabricate linkage — a file
/// with no SLD ref yields no spec method (spec §6.4).
/// What: parses SLD refs from each changed file (first match wins), looks up
/// the spec markdown via `spec_lookup`, and extracts the spec method from the
/// governed section. Returns `(None, None)` when no SLD ref is declared.
///
/// KNOWN C1 LIMITATION (spec §6.4 "first match wins"): the loop returns the
/// FIRST changed file that declares an SLD ref and silently ignores SLD refs in
/// later files. This is intentional for C1 — a PR that touches multiple files
/// each governed by a different spec section is out of scope. Multi-file /
/// multi-section reconciliation (picking the most-specific governing section,
/// or surfacing a conflict across files) is deferred to C4 (#1361), which
/// hardens this seam.
/// Test: `super::tests::spec_resolve_*` (AC-6).
fn resolve_spec(
    changed_files: &[ChangedFile],
    spec_lookup: &dyn SpecLookup,
) -> (Option<SpecRef>, Option<Method>) {
    // First changed file with an SLD ref wins (see KNOWN C1 LIMITATION above).
    for file in changed_files {
        let refs = parse_spec_refs(&file.content);
        if let Some(spec_ref) = refs.into_iter().next() {
            // Look up the spec markdown; a missing file is a gap, not an error
            // (the ISR reads declared links, it does not enforce them).
            let method = spec_lookup
                .load(&spec_ref.file)
                .and_then(|md| extract_spec_method(&md, &spec_ref.anchor));
            return (Some(spec_ref), method);
        }
    }
    (None, None)
}

/// Pluggable spec-markdown loader.
///
/// Why: the spec-resolution leg needs the *text* of a `docs/specs/*.md` file to
/// extract the spec method, but the resolver must not assume a filesystem
/// layout (review's serve mode may load specs differently). This seam keeps the
/// resolver testable offline (spec §6.4, mirrors the token/fetcher seams).
/// What: one method mapping a `docs/specs/*.md` path to its text, or `None`
/// when the spec cannot be loaded (a gap, never an error).
/// Test: `super::tests::spec_resolve_*` uses an in-memory `MapSpecLookup`.
pub trait SpecLookup: Send + Sync {
    /// Load the markdown text for a `docs/specs/*.md` path.
    ///
    /// Why: spec-method extraction needs the section prose (spec §6.4).
    /// What: returns `Some(text)` or `None` when unavailable.
    /// Test: `super::tests::spec_resolve_*`.
    fn load(&self, spec_file: &str) -> Option<String>;
}

/// Assemble a `ResolvedIntent` and apply precedence to it.
///
/// Why: keeps `resolve` readable by separating wiring from the normative
/// precedence computation (spec §6.1).
/// What: builds the struct from the resolved axes, then runs
/// [`apply_precedence`] to set `precedence_winner`/`conflict`/`stale_spec`.
/// Test: `super::tests::precedence_*` (AC-1..AC-4).
fn build_resolved(
    ticket: TicketRef,
    ticket_method: Option<Method>,
    spec_section: Option<SpecRef>,
    spec_method: Option<Method>,
) -> ResolvedIntent {
    let mut intent = ResolvedIntent {
        ticket: Some(ticket),
        ticket_method,
        spec_section,
        spec_method,
        precedence_winner: Precedence::None,
        conflict: false,
        stale_spec: false,
        unresolved: None,
    };
    apply_precedence(&mut intent);
    intent
}

/// Apply the NORMATIVE precedence rule (ticket > spec) to an intent.
///
/// Why: the precedence rule is the heart of the contract and must be applied
/// exactly once, centrally, so no caller re-derives it (spec §6.1, G4).
/// What: implements the four cases verbatim from spec §6.1:
///   1. both present + agree → winner `Ticket`, `conflict=false`.
///   2. both present + disagree → winner `Ticket`, `conflict=true`,
///      `stale_spec=true`.
///   3. ticket only → `Ticket`; spec only → `Spec`.
///   4. neither → `None` (gap).
///
/// Method equality is by normalised `text` (case/whitespace-insensitive).
///
/// Test: `super::tests::precedence_*` (AC-1..AC-4).
pub fn apply_precedence(intent: &mut ResolvedIntent) {
    match (&intent.ticket_method, &intent.spec_method) {
        (Some(t), Some(s)) => {
            intent.precedence_winner = Precedence::Ticket;
            if methods_agree(t, s) {
                intent.conflict = false;
                intent.stale_spec = false;
            } else {
                // Ticket wins; the conflicting spec is stale/advisory.
                intent.conflict = true;
                intent.stale_spec = true;
            }
        }
        (Some(_), None) => {
            intent.precedence_winner = Precedence::Ticket;
        }
        (None, Some(_)) => {
            intent.precedence_winner = Precedence::Spec;
        }
        (None, None) => {
            intent.precedence_winner = Precedence::None;
        }
    }
}

/// Whether a ticket method and a spec method agree.
///
/// Why: case 1 vs. case 2 turns on whether the two methods say the same thing
/// (spec §6.1). A normalised text compare is the conservative C1 rule; richer
/// semantic equivalence is out of C1 scope.
/// What: compares the two methods' `text` after lowercasing and collapsing
/// whitespace; returns `true` when they match.
/// Test: `super::tests::precedence_agree_*` / `precedence_conflict_*`.
fn methods_agree(a: &Method, b: &Method) -> bool {
    normalise(&a.text) == normalise(&b.text)
}

/// Lowercase + collapse internal whitespace for method-text comparison.
///
/// Why: trivial formatting differences must not register as a conflict.
/// What: lowercases, splits on whitespace, and rejoins single-spaced.
/// Test: covered by `precedence_agree_*`.
fn normalise(s: &str) -> String {
    s.to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Convenience wrapper: resolve with the default heuristic extractor.
///
/// Why: the common case wants the OQ-1 default (heuristic, no network) without
/// constructing an extractor by hand (spec §6.2 OQ-1 default path).
/// What: calls [`resolve`] with a [`HeuristicMethodExtractor`].
/// Test: `super::tests` use this wrapper for AC-1..AC-7.
pub async fn resolve_default(
    query: IntentQuery,
    fetcher: &dyn TicketFetcher,
    spec_lookup: &dyn SpecLookup,
) -> ResolvedIntent {
    resolve(query, fetcher, &HeuristicMethodExtractor, spec_lookup).await
}
