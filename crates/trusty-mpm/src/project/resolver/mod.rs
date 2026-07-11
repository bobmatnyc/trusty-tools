//! NL-to-project resolver and session↔project binding (WI-5, #1517).
//!
//! Why: operators work across multiple repos and should not have to remember
//! exact URLs or project names; free-text queries (names, URLs, keywords,
//! ticket IDs) must route automatically to the right registered project so
//! the session manager can spawn or connect sessions without manual lookup.
//! What: exports [`resolve_project`], [`resolve_session_project`], and
//! [`fleet_by_project`] — the three primitives of the NL→project layer:
//! (1) map a free-text query to a ranked [`ProjectResolution`]; (2) bind a
//! live [`SessionRecord`] to its [`Project`]; (3) group active sessions by
//! project into a [`ProjectFleet`] vector for fleet-management UX.
//! Test: unit tests live in the sibling `tests` module (`resolver/tests.rs`),
//! covering exact-name, URL, keyword, multi-candidate disambiguation, clamping,
//! no-match, tie-breaking, and confidence-capping scenarios.

use thiserror::Error;

use super::record::Project;
use crate::session_manager::record::SessionRecord;

// ---------------------------------------------------------------------------
// Public thresholds (spec §4.2 / §5.3, with #1532 clarifications)
// ---------------------------------------------------------------------------

/// Minimum raw keyword score required for a candidate to enter `matches`.
///
/// Why: a single incidental tag hit should not produce a match; 0.3 is exactly
/// one tag-word hit which gives operators a reasonable floor without noise.
/// Per #1532 item 2 this is the *collection* floor, distinct from the
/// disambiguation floor.
/// What: used in the keyword-scoring pass to filter candidates before sorting.
/// Test: `resolver_keyword_threshold_filters_low_score`.
pub const KEYWORD_COLLECTION_FLOOR: f32 = 0.3;

/// Confidence threshold above which multiple candidates trigger disambiguation.
///
/// Why: if more than one candidate scores above this level the caller cannot
/// automatically pick a winner and must surface a disambiguation picker (e.g.
/// in the Telegram bot). Per #1532 item 2 this is the *presentation* floor,
/// separate from the collection floor.
/// What: callers inspect `ProjectResolution::needs_disambiguation()` which uses
/// this constant.
/// Test: `resolver_multi_candidate_disambiguation`, `resolver_single_above_floor`.
pub const DISAMBIGUATION_FLOOR: f32 = 0.6;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors that the NL-to-project resolver can produce.
///
/// Why: structured errors let callers distinguish "no project matched at all"
/// from "the query itself was unusable", enabling different UX responses
/// (disambiguation picker vs. bad-input message).
/// What: three variants cover the no-match case, the empty-registry guard,
/// and future extension points.
/// Test: `resolver_no_match_returns_error`, `resolver_empty_registry`.
#[derive(Debug, Error)]
pub enum ResolverError {
    /// No registered project matched the query even at the collection floor.
    ///
    /// Why: the caller must know there is genuinely no match so it can prompt
    /// the operator to register the project or refine the query.
    /// What: includes the original query string for diagnostic messages.
    #[error("no project matched query: {query:?}")]
    NoMatch { query: String },

    /// The project registry is empty; resolution cannot proceed.
    ///
    /// Why: a separate variant avoids confusing "nothing matched" with "there
    /// was nothing to match against", which require different operator guidance.
    /// What: produced when `projects` slice is empty.
    #[error("project registry is empty; register at least one project first")]
    EmptyRegistry,
}

// ---------------------------------------------------------------------------
// Resolution reason
// ---------------------------------------------------------------------------

/// Why a particular project was chosen by the resolver.
///
/// Why: surfacing the reason lets UIs (Telegram bot, TUI) show operators a
/// brief explanation ("matched by name", "matched URL keyword") and build
/// trust in the routing decision.
/// What: one variant per matching strategy, ordered by precedence.
/// Test: `resolver_exact_name_reason`, `resolver_url_reason`,
/// `resolver_keyword_reason`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolutionReason {
    /// The query was an exact case-insensitive match of the project name.
    ExactNameMatch,
    /// The query contained a URL or URL-like fragment that matched `repo_url`.
    UrlMatch,
    /// Ticket-ID pattern detected; resolved as keyword-only (Phase 1).
    ///
    /// Note: full ticket-tracker integration is deferred to Phase 2 (§7).
    TicketKeyword,
    /// The query words appeared in the project name, description, or tags.
    KeywordMatch,
    /// Semantic embedding similarity (deferred to Phase 2).
    EmbeddingMatch,
}

impl ResolutionReason {
    /// Human-readable description for display in disambiguation UIs.
    ///
    /// Why: UIs that show "matched because…" need a short string per reason.
    /// What: returns a static string slice.
    /// Test: `resolver_reason_label`.
    pub fn label(&self) -> &'static str {
        match self {
            ResolutionReason::ExactNameMatch => "exact name match",
            ResolutionReason::UrlMatch => "URL match",
            ResolutionReason::TicketKeyword => "ticket keyword",
            ResolutionReason::KeywordMatch => "keyword match",
            ResolutionReason::EmbeddingMatch => "embedding similarity",
        }
    }
}

// ---------------------------------------------------------------------------
// Match and resolution types
// ---------------------------------------------------------------------------

/// A single candidate project returned by the resolver.
///
/// Why: callers (Telegram, TUI, MCP tools) need not just the project but also
/// the confidence score and matching reason to present meaningful disambiguation
/// options and audit trails.
/// What: wraps a [`Project`] clone with a clamped `[0.0, 1.0]` confidence
/// score and the [`ResolutionReason`] that explains the match.
/// Test: `resolver_confidence_clamped_to_one`, `resolver_exact_name_confidence`.
#[derive(Debug, Clone)]
pub struct ProjectMatch {
    /// The matched project record.
    pub project: Project,
    /// Confidence score, always in `[0.0, 1.0]`.
    ///
    /// Why: §4.2 documents confidence as "0.0…1.0"; #1532 item 1 requires
    /// clamping because the raw keyword accumulator can exceed 1.0.
    pub confidence: f32,
    /// The strategy that produced this match.
    pub reason: ResolutionReason,
}

/// Resolver output: an ordered list of candidates plus the primary pick.
///
/// Why: returning all candidates enables disambiguation UIs to show a picker
/// while still providing a `primary` shortcut for the common single-match case.
/// What: `matches` is sorted by descending confidence; `primary` is the
/// highest-confidence match. Per #1532 item 3 `primary` is `Option<ProjectMatch>`
/// and is `None` only when the resolver would have returned `NoMatch` — in
/// practice, a `ProjectResolution` is only constructed when ≥1 match exists.
/// Test: `resolver_resolution_primary_is_highest_confidence`,
/// `resolver_multi_candidate_disambiguation`.
#[derive(Debug, Clone)]
pub struct ProjectResolution {
    /// All matching candidates, sorted highest-confidence first.
    pub matches: Vec<ProjectMatch>,
    /// The best candidate. `None` only when `matches` is empty (should not
    /// occur in practice since the resolver returns `Err(NoMatch)` otherwise).
    pub primary: Option<ProjectMatch>,
}

impl ProjectResolution {
    /// Build a `ProjectResolution` from a non-empty sorted match list.
    ///
    /// Why: centralising construction ensures `primary` is always derived from
    /// `matches[0]` and avoids inconsistent field populations at call sites.
    /// What: clones `matches[0]` into `primary`; panics in debug builds if
    /// `matches` is empty (caller invariant).
    /// Test: used by every resolver test via `resolve_project`.
    pub(crate) fn from_sorted(matches: Vec<ProjectMatch>) -> Self {
        let primary = matches.first().cloned();
        Self { matches, primary }
    }

    /// Returns `true` when multiple candidates exceed the disambiguation floor.
    ///
    /// Why: callers can check this flag before presenting a result to decide
    /// whether to auto-route or surface a disambiguation picker.
    /// What: counts matches with `confidence > DISAMBIGUATION_FLOOR`; if > 1
    /// the caller must disambiguate.
    /// Test: `resolver_multi_candidate_disambiguation`,
    /// `resolver_single_above_floor`.
    pub fn needs_disambiguation(&self) -> bool {
        self.matches
            .iter()
            .filter(|m| m.confidence > DISAMBIGUATION_FLOOR)
            .count()
            > 1
    }
}

// ---------------------------------------------------------------------------
// Fleet view
// ---------------------------------------------------------------------------

/// A project and the subset of sessions currently serving it.
///
/// Why: the Telegram `/fleet` command and the TUI both need a per-project
/// grouped view of live sessions; computing it here keeps the rendering layer
/// thin and testable.
/// What: wraps a [`Project`] clone with all [`SessionRecord`]s whose
/// `repo_url` matches the project's `repo_url`.
/// Test: `resolver_fleet_by_project_basic`, `resolver_fleet_empty_sessions`.
#[derive(Debug, Clone)]
pub struct ProjectFleet {
    /// The project that owns these sessions.
    pub project: Project,
    /// All session records bound to this project.
    pub sessions: Vec<SessionRecord>,
}

// ---------------------------------------------------------------------------
// Core resolver functions
// ---------------------------------------------------------------------------

/// Map a free-text query to the best-matching registered project.
///
/// Why: the central entry point for NL→repo routing; all caller surfaces
/// (Telegram free-text, coordinator chat endpoint, MCP `resolve_project` tool)
/// delegate to this function so matching logic is consolidated and testable.
///
/// What: applies three strategies in precedence order — (1) exact name match
/// (confidence 1.0, case-insensitive equality against `Project::name`);
/// (2) URL match (confidence 0.95, extracts repo fragment and matches against
/// `Project::repo_url`); (3) keyword match (confidence clamped to `[0.0, 1.0]`,
/// scores each query word against `name` (+0.4), `description` (+0.2), and
/// `tags` (+0.3); candidates above `KEYWORD_COLLECTION_FLOOR` enter the result
/// set).
///
/// Returns `Err(EmptyRegistry)` when no projects are registered, `Err(NoMatch)`
/// when no candidate reached any floor, or `Ok(ProjectResolution)` with
/// `matches` sorted by descending confidence.
///
/// Test: `resolver_exact_name`, `resolver_url_match`, `resolver_keyword_match`,
/// `resolver_multi_candidate_disambiguation`, `resolver_no_match_returns_error`.
pub fn resolve_project(
    query: &str,
    projects: &[Project],
) -> Result<ProjectResolution, ResolverError> {
    if projects.is_empty() {
        return Err(ResolverError::EmptyRegistry);
    }

    let query_lower = query.trim().to_lowercase();

    // --- Strategy 1: exact name match (highest precedence) ---
    let exact: Vec<ProjectMatch> = projects
        .iter()
        .filter(|p| p.name.to_lowercase() == query_lower)
        .map(|p| ProjectMatch {
            project: p.clone(),
            confidence: 1.0,
            reason: ResolutionReason::ExactNameMatch,
        })
        .collect();

    if !exact.is_empty() {
        return Ok(ProjectResolution::from_sorted(exact));
    }

    // --- Strategy 2: URL match ---
    if is_url_like(&query_lower) {
        let repo_frag = extract_repo_fragment(&query_lower);
        let url_matches: Vec<ProjectMatch> = projects
            .iter()
            .filter(|p| {
                let url_lower = p.repo_url.to_lowercase();
                if let Some(ref frag) = repo_frag {
                    url_lower.contains(frag.as_str())
                } else {
                    url_lower.contains(query_lower.as_str())
                }
            })
            .map(|p| ProjectMatch {
                project: p.clone(),
                confidence: 0.95,
                reason: ResolutionReason::UrlMatch,
            })
            .collect();

        if !url_matches.is_empty() {
            return Ok(ProjectResolution::from_sorted(url_matches));
        }
    }

    // --- Strategy 3: keyword / ticket match ---
    // Detect ticket-ID patterns (e.g. "PROJ-123", "#456").
    let reason = if is_ticket_id(&query_lower) {
        ResolutionReason::TicketKeyword
    } else {
        ResolutionReason::KeywordMatch
    };

    // For ticket IDs, extract the alphabetic prefix as the keyword so that
    // "PROJ-123" scores against projects tagged/named with "proj".  For other
    // queries split on whitespace as usual.
    let ticket_prefix;
    let words: Vec<&str> = if matches!(reason, ResolutionReason::TicketKeyword) {
        ticket_prefix = extract_ticket_prefix(&query_lower);
        vec![ticket_prefix.as_str()]
    } else {
        query_lower.split_whitespace().collect()
    };
    let mut keyword_matches: Vec<ProjectMatch> = projects
        .iter()
        .filter_map(|p| {
            let raw_score = keyword_score(p, &words);
            if raw_score > KEYWORD_COLLECTION_FLOOR {
                // #1532 item 1: clamp to [0.0, 1.0].
                let confidence = raw_score.clamp(0.0, 1.0);
                Some(ProjectMatch {
                    project: p.clone(),
                    confidence,
                    reason: reason.clone(),
                })
            } else {
                None
            }
        })
        .collect();

    keyword_matches.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    if !keyword_matches.is_empty() {
        return Ok(ProjectResolution::from_sorted(keyword_matches));
    }

    Err(ResolverError::NoMatch {
        query: query.to_string(),
    })
}

/// Find the registered project whose `repo_url` matches `repo_url` exactly
/// (case-insensitive, `.git`-suffix-normalised).
///
/// Why: both a live session ([`resolve_session_project`]) and a pre-spawn
/// request that only carries a bare `repo_url` (the Deliverable-linkage
/// validation in `daemon::managed_routes::spawn_session`, DOC-35 §10.6,
/// #2379) need to bind a repo URL to its registry-B project name; extracting
/// the shared match rule keeps the normalisation logic in exactly one place
/// rather than duplicating it at the new call site.
/// What: normalises both sides and returns the first exact match. Per #1532
/// item 5, when the same `repo_url` is registered under multiple project
/// names the first match in `projects` order wins.
/// Test: `resolver_by_repo_url_matches`, `resolver_by_repo_url_no_match`
/// (`resolve_session_project`'s own tests exercise this transitively).
pub fn resolve_project_by_repo_url<'a>(
    repo_url: &str,
    projects: &'a [Project],
) -> Option<&'a Project> {
    let repo_lower = repo_url.to_lowercase();
    let repo_norm = normalise_url(&repo_lower);
    projects.iter().find(|p| {
        let proj_lower = p.repo_url.to_lowercase();
        normalise_url(&proj_lower) == repo_norm
    })
}

/// Bind a live session to its registered project by `repo_url`.
///
/// Why: `SessionRecord` carries `repo_url` but not a project name reference;
/// this function performs the lookup so fleet grouping and focused-session
/// routing do not need to replicate the matching logic.
/// What: delegates to [`resolve_project_by_repo_url`] once the session's
/// `repo_url` is confirmed present.
/// Test: `resolver_session_project_binding`, `resolver_session_no_repo`,
/// `resolver_session_url_tie_break`.
pub fn resolve_session_project<'a>(
    session: &SessionRecord,
    projects: &'a [Project],
) -> Option<&'a Project> {
    let session_url = session.repo_url.as_ref()?;
    resolve_project_by_repo_url(session_url, projects)
}

/// Group a slice of session records by their bound project.
///
/// Why: the Telegram `/fleet` command and the SM TUI both need a
/// project-grouped view of active sessions; computing it once here keeps
/// the rendering layer thin.
/// What: for each registered project collects sessions whose `repo_url`
/// matches; projects with no sessions are included with an empty `sessions`
/// vec so callers can still display the full registry. Session records with
/// no `repo_url` (or whose URL matches no project) are silently omitted from
/// the fleet view.
/// Test: `resolver_fleet_by_project_basic`, `resolver_fleet_empty_sessions`,
/// `resolver_fleet_no_repo_sessions_omitted`.
pub fn fleet_by_project(sessions: &[SessionRecord], projects: &[Project]) -> Vec<ProjectFleet> {
    projects
        .iter()
        .map(|project| {
            let bound: Vec<SessionRecord> = sessions
                .iter()
                .filter(|s| resolve_session_project(s, std::slice::from_ref(project)).is_some())
                .cloned()
                .collect();
            ProjectFleet {
                project: project.clone(),
                sessions: bound,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Returns `true` if the query looks like a URL or URL fragment.
///
/// Why: URL-matching must not be triggered for short keyword queries;
/// requiring "://" or a leading "github.com/" prefix keeps false-positives low.
/// What: checks for "://" (standard URL scheme) or "github.com/" prefix after
/// stripping whitespace.
/// Test: covered via URL-match resolver tests.
fn is_url_like(query: &str) -> bool {
    query.contains("://") || query.starts_with("github.com/")
}

/// Extract a `owner/repo` or bare `repo` fragment from a URL string.
///
/// Why: matching a full URL against `repo_url` is fragile (http vs https,
/// trailing `.git`, port numbers); extracting the repo fragment and doing a
/// substring match is more robust.
/// What: splits on "/" and takes the last two non-empty segments (e.g.
/// `owner/repo`); strips a trailing ".git" if present.
/// Test: covered via `resolver_url_match` tests.
fn extract_repo_fragment(url: &str) -> Option<String> {
    // Strip the scheme prefix if present.
    let path = if let Some(pos) = url.find("://") {
        &url[pos + 3..]
    } else {
        url
    };
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if segments.len() < 2 {
        // Only one segment — just return it stripped of ".git".
        return segments
            .last()
            .map(|s| s.strip_suffix(".git").unwrap_or(s).to_string());
    }
    let n = segments.len();
    let owner = segments[n - 2];
    let repo = segments[n - 1]
        .strip_suffix(".git")
        .unwrap_or(segments[n - 1]);
    Some(format!("{owner}/{repo}"))
}

/// Returns `true` if the query looks like a ticket identifier.
///
/// Why: ticket IDs (`PROJ-123`, `#456`) should be flagged so the resolver
/// can attach a `TicketKeyword` reason even though full ticket-tracker
/// integration is deferred to Phase 2.
/// What: matches patterns like `ABC-123` or `#123`.
/// Test: `resolver_ticket_keyword_reason`.
fn is_ticket_id(query: &str) -> bool {
    // Pattern: one or more uppercase letters, a dash, one or more digits.
    let bytes = query.as_bytes();
    // Also accept leading "#" for GitHub issue refs.
    if query.starts_with('#') && query[1..].chars().all(|c| c.is_ascii_digit()) {
        return true;
    }
    // PROJ-123 pattern: letters then dash then digits.
    let dash_pos = bytes.iter().position(|&b| b == b'-');
    if let Some(pos) = dash_pos {
        let prefix = &query[..pos];
        let suffix = &query[pos + 1..];
        prefix.chars().all(|c| c.is_ascii_alphabetic())
            && !suffix.is_empty()
            && suffix.chars().all(|c| c.is_ascii_digit())
    } else {
        false
    }
}

/// Extract the alphabetic project-code prefix from a ticket-ID string.
///
/// Why: ticket IDs like `"PROJ-123"` or `"#456"` should score against the
/// project-code fragment (`"proj"`, `"#"` stripped) rather than the full
/// token so that projects tagged or named with that code get matched.
/// What: for PROJ-NNN form, returns everything before the dash (lowercased);
/// for `#NNN` form, returns the string `"#"` (minimal; callers that want
/// GitHub-issue routing should integrate the issue tracker in Phase 2).
/// Test: covered by `resolver_ticket_keyword_reason`.
fn extract_ticket_prefix(ticket_lower: &str) -> String {
    if ticket_lower.starts_with('#') {
        return "#".to_string();
    }
    if let Some(dash_pos) = ticket_lower.find('-') {
        return ticket_lower[..dash_pos].to_string();
    }
    ticket_lower.to_string()
}

/// Accumulate a keyword relevance score for `project` given the query `words`.
///
/// Why: the score drives ranking; per §5.3 each word contributes per-field
/// weights, and per #1532 item 1 the total must be clamped by the caller.
///
/// What: returns the raw (potentially > 1.0) sum; the caller clamps to
/// `[0.0, 1.0]`. Per-word weights: name hit +0.4, description hit +0.2,
/// exact tag match +0.3.
///
/// Note on description-only matches: a query word that matches only the
/// description scores 0.2, which is below `KEYWORD_COLLECTION_FLOOR` (0.3).
/// Description-only hits therefore never surface as a result on their own —
/// they only contribute to the final score when a name or tag hit also fires.
/// This is intentional (Phase 1 scope); raise the description weight or lower
/// the collection floor in a future phase if description-only routing is needed.
///
/// Test: `resolver_keyword_score_accumulates`, `resolver_keyword_clamped`.
fn keyword_score(project: &Project, words: &[&str]) -> f32 {
    let name_lower = project.name.to_lowercase();
    let desc_lower = project.description.as_deref().unwrap_or("").to_lowercase();
    let tags_lower: Vec<String> = project.tags.iter().map(|t| t.to_lowercase()).collect();

    let mut score = 0.0_f32;
    for &word in words {
        if name_lower.contains(word) {
            score += 0.4;
        }
        if !desc_lower.is_empty() && desc_lower.contains(word) {
            score += 0.2;
        }
        for tag in &tags_lower {
            if tag == word {
                score += 0.3;
                break; // count each tag at most once per word
            }
        }
    }
    score
}

/// Normalise a repository URL for fuzzy equality comparison.
///
/// Why: two URLs for the same repo may differ by trailing ".git" or trailing
/// slash; normalising before comparison avoids false non-matches.
/// What: strips a trailing "/" then a trailing ".git" suffix.
/// Test: covered via `resolve_session_project` tests.
fn normalise_url(url: &str) -> &str {
    let u = url.trim_end_matches('/');
    u.strip_suffix(".git").unwrap_or(u)
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
