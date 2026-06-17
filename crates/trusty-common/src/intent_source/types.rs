//! Data contract for the intent-source resolver (ISR).
//!
//! Why: both conformance gates (FRONT in trusty-mpm, BACK in trusty-review)
//! must agree on a *single* shape for "the resolved intent" and on how
//! precedence (ticket > spec) was applied. Defining the contract once here —
//! in the crate both gates already depend on — guarantees the two gates can
//! never disagree about which intent source wins (spec §6.1, §6.5).
//! What: the input query (`IntentQuery`), the output (`ResolvedIntent`) and its
//! constituent value types (`Method`, `MethodKind`, `Precedence`, `TicketRef`,
//! `SpecRef`), plus the resolver error type (`IsrError`).
//! Test: `super::tests` — round-trip + constructor coverage (AC-1..AC-7).

use serde::{Deserialize, Serialize};

/// Input to [`crate::intent_source::resolve`]: one of two query shapes.
///
/// Why: the BACK gate starts from a PR (it must *extract* the ticket-id and
/// resolve spec links from changed files), while the FRONT gate already holds
/// the ticket-id pre-work. Modelling both as one enum keeps a single resolver
/// entry point (spec §6.1).
/// What: `Pr` carries owner/repo/pr-number plus the diff/changed-file context
/// used for ticket-linkage and spec-resolution; `Ticket` carries a known
/// ticket-id and optional changed files.
/// Test: `super::tests::pr_query_*` / `ticket_query_*`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntentQuery {
    /// BACK gate: resolve intent from a pull request (extract ticket + spec).
    Pr {
        /// Repository owner (`bobmatnyc`), used for ticket fetch.
        owner: String,
        /// Repository name (`trusty-tools`), used for ticket fetch.
        repo: String,
        /// The PR number (informational; linkage keys off body/commits/branch).
        pr_number: u64,
        /// PR body / description — the primary `Closes #N` linkage source.
        body: String,
        /// Branch name (e.g. `fix/1325-x`) — a fallback linkage source.
        branch: Option<String>,
        /// Commit messages in the PR — a secondary linkage source.
        commit_messages: Vec<String>,
        /// Changed file paths + their source text, for SLD spec-resolution.
        changed_files: Vec<ChangedFile>,
    },
    /// FRONT gate: resolve intent from a known ticket-id.
    Ticket {
        /// The backend-native ticket id (`#1358`, `ENG-12`, …).
        ticket_id: String,
        /// Repository owner, used to select/scope the ticket backend.
        owner: String,
        /// Repository name, used to select/scope the ticket backend.
        repo: String,
        /// Changed file paths + source (may be empty pre-work) for spec links.
        changed_files: Vec<ChangedFile>,
    },
}

/// A changed file paired with the source text the ISR scans for SLD refs.
///
/// Why: SLD spec-resolution (spec §6.4) reads `# Spec References` rustdoc
/// blocks out of the *content* of changed files — a path alone is not enough.
/// What: `path` is the repo-relative path; `content` is the file's text (the
/// caller supplies it; the ISR never reads the filesystem itself).
/// Test: `super::tests::spec_resolve_*`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedFile {
    /// Repo-relative path (`crates/trusty-common/src/intent_source/mod.rs`).
    pub path: String,
    /// Full source text of the file, scanned for SLD docstring references.
    pub content: String,
}

/// Which intent source won under the fixed precedence rule (ticket > spec).
///
/// Why: callers must know, without re-deriving, whether the ticket or the spec
/// is the authority for this unit of work — and `None` signals a genuine gap
/// (spec §6.1 precedence resolution).
/// What: a three-state marker serialised lowercase for stable wire shape.
/// Test: `super::tests::precedence_*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Precedence {
    /// The ticket specifies the method (wins over any spec).
    Ticket,
    /// Only the spec specifies the method (ticket silent).
    Spec,
    /// Neither specifies a method — a gap, nothing to conform to.
    None,
}

/// The class of an extracted method statement.
///
/// Why: a conformance gate downstream may treat a hard constraint ("no new
/// dependency") differently from an approach hint ("prefer cursor pagination");
/// tagging the kind keeps that option open without re-parsing the text.
/// What: a coarse taxonomy over the imperative-method phrasings the heuristic
/// extractor recognises (spec §6.2). `Unspecified` is the safe default when a
/// method was supplied verbatim (e.g. by an LLM) without a class.
/// Test: `super::tests::extract_*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MethodKind {
    /// A prescribed approach/technique ("use cursor-based pagination").
    Approach,
    /// A prohibition / hard constraint ("no new dependency", "must not block").
    Constraint,
    /// A reuse directive ("reuse the existing `ContextSource` trait").
    Reuse,
    /// Method text supplied without a recognised class.
    Unspecified,
}

/// An extracted statement of approach/technique/constraint.
///
/// Why: the matrix (spec §4) compares the *subject* against a *method*, not
/// against free prose. Carrying the verbatim source excerpt keeps the finding
/// explainable and auditable (no hallucinated constraints — spec §6.2).
/// What: `text` is the normalised method statement; `kind` classifies it;
/// `source_excerpt` is the verbatim line(s) it was lifted from.
/// Test: `super::tests::extract_*`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Method {
    /// Normalised, human-readable statement of the method.
    pub text: String,
    /// Coarse classification of the method statement.
    pub kind: MethodKind,
    /// Verbatim excerpt the method was extracted from (for explainability).
    pub source_excerpt: String,
}

/// A reference to the ticket the intent was resolved from.
///
/// Why: the BACK gate must cite *which* ticket it checked conformance against;
/// the FRONT gate echoes it into escalation context (spec §6.1 outputs).
/// What: the backend-native id, human title, optional URL, and backend tag.
/// Test: `super::tests::ticket_query_*`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketRef {
    /// Backend-native id (`#1358`, `ENG-12`).
    pub id: String,
    /// Human-readable ticket title.
    pub title: String,
    /// Canonical URL, when the backend supplied one.
    pub url: Option<String>,
    /// Backend identifier (`github` / `jira` / `linear`).
    pub backend: String,
}

/// A reference to the spec section the intent was resolved from.
///
/// Why: when the spec axis wins (or conflicts), the gate must cite the exact
/// `SPEC-…` anchor + file it checked against (spec §6.1, §6.4).
/// What: the spec id (`SPEC-SUBSYSTEM-NN~vR`), the `docs/specs/*.md` file, and
/// the in-file anchor (`SPEC-SUBSYSTEM-NN`).
/// Test: `super::tests::spec_resolve_*`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecRef {
    /// The full spec id including revision (`SPEC-CONFORMANCE-03~draft`).
    pub spec_id: String,
    /// The `docs/specs/*.md` file the anchor lives in.
    pub file: String,
    /// The in-file heading anchor (`SPEC-CONFORMANCE-03~draft`).
    pub anchor: String,
}

/// The resolved intent — the single contract both gates consume.
///
/// Why: centralising precedence here (applied exactly once) guarantees FRONT
/// and BACK never disagree about which source wins (spec §6.1, G4).
/// What: the ticket + ticket method, the spec section + spec method, the
/// precedence winner, the `conflict`/`stale_spec` flags, and an `unresolved`
/// fail-open reason. When `unresolved` is `Some`, all method fields are `None`
/// and both gates no-op (spec §4.2).
/// Test: `super::tests` — every AC-1..AC-7 case asserts on this struct.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedIntent {
    /// The linked ticket, if one was resolved.
    pub ticket: Option<TicketRef>,
    /// The method prescribed by the ticket, if any.
    pub ticket_method: Option<Method>,
    /// The governing spec section, if one was resolved via SLD links.
    pub spec_section: Option<SpecRef>,
    /// The method prescribed by the spec, if any.
    pub spec_method: Option<Method>,
    /// Which source won under precedence (ticket > spec).
    pub precedence_winner: Precedence,
    /// True when ticket and spec both specify a method and they disagree.
    pub conflict: bool,
    /// True when a conflicting spec was downgraded to advisory under the
    /// ticket's precedence.
    pub stale_spec: bool,
    /// Fail-open reason: `Some` when resolution could not complete. All method
    /// fields are `None` in that case.
    pub unresolved: Option<String>,
}

impl ResolvedIntent {
    /// Construct the "no intent" result for non-ticketed work.
    ///
    /// Why: non-ticketed work has no intent to conform to; both gates must
    /// no-op on it (spec §4.2 preconditions). A named constructor makes the
    /// intent explicit at every call site.
    /// What: returns a `ResolvedIntent` with every field empty/`None`,
    /// `precedence_winner = None`, and `unresolved = None` (this is a *clean*
    /// gap, not a failure).
    /// Test: `super::tests::none_is_gap`.
    #[must_use]
    pub fn none() -> Self {
        Self {
            ticket: None,
            ticket_method: None,
            spec_section: None,
            spec_method: None,
            precedence_winner: Precedence::None,
            conflict: false,
            stale_spec: false,
            unresolved: None,
        }
    }

    /// Construct a fail-open result carrying the failure reason.
    ///
    /// Why: any fetch/parse failure must degrade to "no intent" rather than
    /// panic or block work (spec §4.2 error conditions, §6.1).
    /// What: returns a `ResolvedIntent` like [`ResolvedIntent::none`] but with
    /// `unresolved = Some(reason)` so callers can log/surface the cause.
    /// Test: `super::tests::unresolved_carries_reason`.
    #[must_use]
    pub fn unresolved(reason: impl Into<String>) -> Self {
        Self {
            unresolved: Some(reason.into()),
            ..Self::none()
        }
    }

    /// True when neither ticket nor spec prescribed a method (a gap).
    ///
    /// Why: the matrix's M3 row keys off "no method on either axis"; a helper
    /// keeps gate code declarative (spec §4.1 M3).
    /// What: returns `true` iff both `ticket_method` and `spec_method` are
    /// `None` (independent of `unresolved`).
    /// Test: `super::tests::none_is_gap`.
    #[must_use]
    pub fn is_gap(&self) -> bool {
        self.ticket_method.is_none() && self.spec_method.is_none()
    }
}

/// Errors that can arise inside the resolver before the fail-open conversion.
///
/// Why: trusty-common is a library, so internal failure modes are modelled as
/// a structured `thiserror` enum rather than `anyhow` (CLAUDE.md convention).
/// The public `resolve` entry never returns this — it maps every variant into
/// `ResolvedIntent::unresolved` (spec §4.2 fail-open) — but the typed error
/// keeps the internal plumbing precise and testable.
/// What: the failure categories the resolver distinguishes.
/// Test: `super::tests::error_display_*`.
#[derive(Debug, thiserror::Error)]
pub enum IsrError {
    /// No ticket-id could be extracted from the PR (body/commits/branch).
    #[error("no ticket linkage found in PR")]
    NoTicketLinkage,
    /// The ticket backend could not fetch the issue.
    #[error("ticket fetch failed: {0}")]
    TicketFetch(String),
    /// A GitHub access token could not be resolved for the fetch.
    #[error("no auth token available: {0}")]
    NoToken(String),
}
