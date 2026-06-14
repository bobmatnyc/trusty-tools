//! Unified project-management adapter trait.
//!
//! This module defines a common interface — [`PmAdapter`] — that abstracts
//! over the various PM/ticketing systems we integrate with (JIRA, GitHub
//! Issues, Linear, Azure DevOps). The goal is to let the classify/collect
//! pipeline enrich commits with ticket metadata without caring which backend
//! is actually serving the data.
//!
//! ## Architecture
//!
//! Each PM client implements [`PmAdapter`] and exposes:
//! - [`fetch_ticket`](PmAdapter::fetch_ticket) — single-ticket lookup (returns
//!   `Ok(None)` for "not found", reserving `Err(_)` for transport/auth errors).
//! - [`fetch_tickets`](PmAdapter::fetch_tickets) — batch lookup (default
//!   implementation runs sequentially; adapters with native batch endpoints
//!   should override).
//! - [`detect_ticket_refs`](PmAdapter::detect_ticket_refs) — recognize
//!   ticket-shaped strings (e.g. `PROJ-123`, `#42`, `AB#7`) in arbitrary text.
//! - [`health_check`](PmAdapter::health_check) — connectivity / auth probe.
//!
//! All ticket payloads are normalized to [`PmTicket`] — the system-specific
//! response JSON is preserved in [`PmTicket::raw`] for forward compatibility.
//!
//! ## Factory
//!
//! [`build_adapters`] instantiates every PM adapter that is configured in the
//! supplied [`Config`]. Adapters whose config is absent or invalid are simply
//! skipped (with a `tracing::warn!`) so the caller does not have to know which
//! integrations are enabled.

use std::sync::OnceLock;

use async_trait::async_trait;
use regex::Regex;
use serde::{Deserialize, Serialize};
use tracing::warn;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Source PM system that produced a [`PmTicket`].
///
/// Used by downstream consumers (reports, classification rules) to apply
/// system-specific logic — e.g. distinguishing `AB#42` (ADO) from `#42`
/// (GitHub) when both could appear in the same commit corpus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PmSource {
    /// Atlassian JIRA (Cloud or Server).
    Jira,
    /// GitHub Issues / Pull Requests.
    GitHub,
    /// Linear (linear.app).
    Linear,
    /// Microsoft Azure DevOps (work items).
    AzureDevOps,
}

impl PmSource {
    /// Stable, lowercase string label suitable for logs, DB rows, and report
    /// columns.
    pub fn as_str(&self) -> &'static str {
        match self {
            PmSource::Jira => "jira",
            PmSource::GitHub => "github",
            PmSource::Linear => "linear",
            PmSource::AzureDevOps => "azure_devops",
        }
    }
}

/// Normalized ticket payload returned by every [`PmAdapter`] implementation.
///
/// Fields that don't exist in a given source system are filled with sensible
/// defaults (`""` for strings, empty `Vec` for `labels`, `None` for `url`).
/// The full upstream JSON is preserved verbatim in [`PmTicket::raw`] so callers
/// that need backend-specific fields (e.g. JIRA custom fields) don't have to
/// re-fetch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PmTicket {
    /// Canonical ticket identifier as reported by the PM system
    /// (e.g. `"PROJ-123"`, `"#42"`, `"AB#7"`).
    pub id: String,
    /// Short human-readable title / summary.
    pub title: String,
    /// Current workflow status (e.g. `"Done"`, `"In Progress"`, `"closed"`).
    pub status: String,
    /// Issue type / classification (e.g. `"story"`, `"bug"`, `"task"`,
    /// `"epic"`). Backends that don't expose this concept return `""`.
    pub ticket_type: String,
    /// Labels / tags. Empty when unavailable.
    pub labels: Vec<String>,
    /// Web URL to the ticket in the PM system, if known.
    pub url: Option<String>,
    /// Source PM system this ticket originated from.
    pub source: PmSource,
    /// Raw upstream payload — preserved for forward compatibility and for
    /// downstream consumers that need fields not in the normalized struct.
    pub raw: serde_json::Value,
}

/// Errors returned by [`PmAdapter`] implementations.
///
/// `From` conversions exist for the common low-level error types so that
/// implementations can use `?` against `reqwest::Error`, `serde_json::Error`,
/// etc. without manual mapping.
#[derive(Debug, thiserror::Error)]
pub enum PmError {
    /// HTTP transport or response error.
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    /// Authentication failed — bad credentials, missing token, expired PAT, …
    #[error("authentication failed for {system}: {message}")]
    Auth {
        /// System label (see [`PmSource::as_str`]).
        system: String,
        /// Human-readable detail.
        message: String,
    },

    /// Ticket not found. Adapters should prefer returning `Ok(None)` for the
    /// "looked but didn't find it" case; this variant is for situations where
    /// not-found is genuinely an error condition (e.g. an explicit lookup-by-id
    /// API where the caller asserted the ticket exists).
    #[error("ticket not found: {id}")]
    NotFound {
        /// Ticket identifier that was looked up.
        id: String,
    },

    /// Rate-limited by the upstream system.
    #[error("rate limited by {system}")]
    RateLimited {
        /// System label (see [`PmSource::as_str`]).
        system: String,
    },

    /// JSON serialization/deserialization failure.
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// Configuration was missing or invalid for the requested operation.
    #[error("configuration error for {system}: {message}")]
    Config {
        /// System label (see [`PmSource::as_str`]).
        system: String,
        /// Human-readable detail.
        message: String,
    },

    /// Catch-all for backend-specific errors that don't fit the variants above.
    #[error("{system}: {message}")]
    Other {
        /// System label (see [`PmSource::as_str`]).
        system: String,
        /// Human-readable detail.
        message: String,
    },
}

// ---------------------------------------------------------------------------
// The trait
// ---------------------------------------------------------------------------

/// Common interface for all PM system clients.
///
/// Implementations must be `Send + Sync` so adapters can be stored in a
/// `Vec<Box<dyn PmAdapter>>` and shared across the async pipeline.
#[async_trait]
pub trait PmAdapter: Send + Sync {
    /// Stable, lowercase name of the PM system (e.g. `"jira"`, `"github"`,
    /// `"linear"`, `"azure_devops"`). Used for logging and error messages.
    fn name(&self) -> &str;

    /// Source enum corresponding to [`name`](Self::name).
    fn source(&self) -> PmSource;

    /// Fetch a ticket by its system-native identifier.
    ///
    /// Returns:
    /// - `Ok(Some(ticket))` on success.
    /// - `Ok(None)` if the ticket does not exist or is not visible with the
    ///   configured credentials (i.e. an authoritative "not found").
    /// - `Err(_)` on transport, auth, parsing, or rate-limit failures.
    async fn fetch_ticket(&self, ticket_id: &str) -> Result<Option<PmTicket>, PmError>;

    /// Batch-fetch multiple tickets.
    ///
    /// The default implementation calls [`fetch_ticket`](Self::fetch_ticket)
    /// sequentially. Adapters with a native batch endpoint (e.g. JIRA's
    /// `/search`, ADO's `/workitemsbatch`) should override for efficiency.
    async fn fetch_tickets(&self, ticket_ids: &[&str]) -> Vec<Result<Option<PmTicket>, PmError>> {
        let mut out = Vec::with_capacity(ticket_ids.len());
        for id in ticket_ids {
            out.push(self.fetch_ticket(id).await);
        }
        out
    }

    /// Detect strings in `text` that look like ticket references for this
    /// system. Each adapter scopes its detection to its own format —
    /// e.g. JIRA matches `[A-Z][A-Z0-9]*-\d+`, GitHub matches `#\d+`,
    /// ADO matches `AB#\d+`.
    ///
    /// Returns the deduplicated list of matches in first-seen order.
    fn detect_ticket_refs(&self, text: &str) -> Vec<String>;

    /// Test connectivity and authentication against the upstream system.
    ///
    /// Implementations should perform a cheap call (e.g. `GET /myself` for
    /// JIRA, `GET _apis/connectionData` for ADO) and return `Ok(())` on
    /// success.
    async fn health_check(&self) -> Result<(), PmError>;
}

// ---------------------------------------------------------------------------
// Detection helpers (shared regex set)
// ---------------------------------------------------------------------------

/// Lazily-compiled JIRA / Linear identifier regex (`[A-Z][A-Z0-9]*-\d+`).
fn jira_ref_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\b([A-Z][A-Z0-9]{0,9})-(\d+)\b").expect("jira regex compiles"))
}

/// Lazily-compiled GitHub bare-issue regex (`#\d+` after start-of-line or
/// whitespace, so we don't match hex colors).
fn github_ref_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?m)(?:^|\s)(#\d+)\b").expect("github regex compiles"))
}

/// Lazily-compiled Azure DevOps regex (`AB#\d+`).
fn azdo_ref_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\b(AB#\d+)\b").expect("azdo regex compiles"))
}

/// Compile a user-supplied ticket-detection regex.
///
/// Why: lets users override the hardcoded JIRA / GitHub / Linear detection
/// patterns to accommodate real-world commit-message conventions
/// (lowercase keys, longer prefixes, `Fix:#123`, etc.) without code changes.
/// What: attempts to compile `pattern`; on compile failure or when the
/// regex has zero capture groups, emits a `tracing::warn!` and returns
/// `None` so the caller falls back to the default pattern.
/// Test: assert that `compile_user_regex("x", Some("\\d+"))` returns `None`
/// (no capture group), and that `compile_user_regex("x", Some("(\\d+)"))`
/// returns `Some(_)`.
fn compile_user_regex(system: &str, pattern: Option<&str>) -> Option<Regex> {
    let pat = pattern?;
    match Regex::new(pat) {
        Ok(re) => {
            if re.captures_len() < 2 {
                warn!(
                    system = system,
                    pattern = pat,
                    "ticket_regex has no capture group; ignoring and using default pattern"
                );
                None
            } else {
                Some(re)
            }
        }
        Err(e) => {
            // Should be unreachable when called from build_adapters because
            // Config::load already validates compilability — kept for defense
            // in depth and for callers that construct adapters by hand.
            warn!(
                system = system,
                pattern = pat,
                error = %e,
                "ticket_regex failed to compile; using default pattern"
            );
            None
        }
    }
}

/// Extract deduplicated matches for the user-supplied regex's first capture
/// group from `text`.
///
/// Why: user-supplied regexes always expose the ticket ID in group 1; this
/// differs from the built-in JIRA pattern, which uses two groups and joins
/// them. Keeping the logic separate avoids over-generalizing
/// [`extract_unique`].
fn extract_user_regex(re: &Regex, text: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for cap in re.captures_iter(text) {
        if let Some(m) = cap.get(1) {
            let s = m.as_str().to_string();
            if seen.insert(s.clone()) {
                out.push(s);
            }
        }
    }
    out
}

/// Extract deduplicated matches for `re`'s first capture group from `text`.
fn extract_unique(re: &Regex, text: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for cap in re.captures_iter(text) {
        // Some patterns have one group (whole match), some have two.
        let m = cap.get(1).map(|m| m.as_str().to_string());
        if let Some(s) = m {
            // For JIRA pattern we want the full "KEY-N", not just "KEY".
            // Detect by checking if there's a 2nd group.
            let full = if cap.len() > 2 {
                match (cap.get(1), cap.get(2)) {
                    (Some(a), Some(b)) => format!("{}-{}", a.as_str(), b.as_str()),
                    _ => s,
                }
            } else {
                s
            };
            if seen.insert(full.clone()) {
                out.push(full);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Adapter implementations
// ---------------------------------------------------------------------------

/// PM adapter wrapping [`crate::collect::jira::JiraClient`].
pub struct JiraAdapter {
    inner: crate::collect::jira::JiraClient,
    /// Optional user-supplied detection regex. When `None`, the adapter
    /// falls back to the shared default JIRA pattern.
    ticket_regex: Option<Regex>,
}

impl JiraAdapter {
    /// Construct from an existing [`crate::collect::jira::JiraClient`]
    /// using the default JIRA detection regex.
    pub fn new(inner: crate::collect::jira::JiraClient) -> Self {
        Self {
            inner,
            ticket_regex: None,
        }
    }

    /// Construct from a client and an optional user-supplied detection regex
    /// string. The string is pre-validated at config-load time, so a parse
    /// failure here is treated as a non-fatal warning and the adapter falls
    /// back to the default pattern. A regex with no capture groups is also
    /// rejected with a warning.
    pub fn with_ticket_regex(
        inner: crate::collect::jira::JiraClient,
        pattern: Option<&str>,
    ) -> Self {
        Self {
            inner,
            ticket_regex: compile_user_regex("jira", pattern),
        }
    }
}

#[async_trait]
impl PmAdapter for JiraAdapter {
    fn name(&self) -> &str {
        "jira"
    }

    fn source(&self) -> PmSource {
        PmSource::Jira
    }

    async fn fetch_ticket(&self, ticket_id: &str) -> Result<Option<PmTicket>, PmError> {
        match self.inner.fetch_issue(ticket_id).await {
            Ok(Some(issue)) => {
                let raw = serde_json::json!({
                    "key": issue.key,
                    "summary": issue.summary,
                    "status": issue.status,
                    "issuetype": issue.issue_type,
                });
                Ok(Some(PmTicket {
                    id: issue.key,
                    title: issue.summary,
                    status: issue.status,
                    ticket_type: issue.issue_type,
                    labels: Vec::new(),
                    url: None,
                    source: PmSource::Jira,
                    raw,
                }))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(collect_err_to_pm("jira", e)),
        }
    }

    fn detect_ticket_refs(&self, text: &str) -> Vec<String> {
        match &self.ticket_regex {
            Some(re) => extract_user_regex(re, text),
            None => extract_unique(jira_ref_re(), text),
        }
    }

    async fn health_check(&self) -> Result<(), PmError> {
        // No dedicated health endpoint wired yet — issue a benign lookup that
        // returns Ok(None) on 404 and Err on transport/auth failure.
        match self.inner.fetch_issue("HEALTH-0").await {
            Ok(_) => Ok(()),
            Err(e) => Err(collect_err_to_pm("jira", e)),
        }
    }
}

/// PM adapter wrapping [`crate::collect::github::GitHubClient`].
///
/// GitHub's `Issues` API is a superset of its `Pulls` API — both share the
/// `#N` namespace. `fetch_ticket` accepts either `"#42"` or `"42"` and
/// delegates to [`crate::collect::github::GitHubClient::fetch_issue`].
pub struct GitHubAdapter {
    inner: crate::collect::github::GitHubClient,
    /// Optional user-supplied detection regex. When `None`, the adapter
    /// falls back to the shared default GitHub pattern.
    ticket_regex: Option<Regex>,
}

impl GitHubAdapter {
    /// Construct from an existing [`crate::collect::github::GitHubClient`]
    /// using the default GitHub detection regex.
    pub fn new(inner: crate::collect::github::GitHubClient) -> Self {
        Self {
            inner,
            ticket_regex: None,
        }
    }

    /// Construct from a client and an optional user-supplied detection regex.
    /// See [`JiraAdapter::with_ticket_regex`] for semantics.
    pub fn with_ticket_regex(
        inner: crate::collect::github::GitHubClient,
        pattern: Option<&str>,
    ) -> Self {
        Self {
            inner,
            ticket_regex: compile_user_regex("github", pattern),
        }
    }
}

#[async_trait]
impl PmAdapter for GitHubAdapter {
    fn name(&self) -> &str {
        "github"
    }

    fn source(&self) -> PmSource {
        PmSource::GitHub
    }

    async fn fetch_ticket(&self, ticket_id: &str) -> Result<Option<PmTicket>, PmError> {
        // GitHub ticket refs may carry a leading `#`. Strip it so callers
        // can pass either `#42` or `42`. A non-numeric id is treated as
        // "not a GitHub issue" → `Ok(None)`.
        let numeric = ticket_id.trim_start_matches('#');
        let number: u64 = match numeric.parse() {
            Ok(n) => n,
            Err(_) => return Ok(None),
        };

        match self.inner.fetch_issue(number).await {
            Ok(Some(issue)) => {
                let labels: Vec<String> = issue.labels.iter().map(|l| l.name.clone()).collect();
                let url = issue.html_url.clone();
                let raw = serde_json::to_value(&issue)?;
                Ok(Some(PmTicket {
                    id: format!("#{}", issue.number),
                    title: issue.title,
                    status: issue.state,
                    ticket_type: "issue".into(),
                    labels,
                    url: Some(url),
                    source: PmSource::GitHub,
                    raw,
                }))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(collect_err_to_pm("github", e)),
        }
    }

    fn detect_ticket_refs(&self, text: &str) -> Vec<String> {
        match &self.ticket_regex {
            Some(re) => extract_user_regex(re, text),
            None => extract_unique(github_ref_re(), text),
        }
    }

    async fn health_check(&self) -> Result<(), PmError> {
        // Token-presence check — until a dedicated `/zen` ping is added,
        // we just assert that *some* token is configured.
        if self.inner.has_token() {
            Ok(())
        } else {
            Err(PmError::Auth {
                system: "github".into(),
                message: "no token configured".into(),
            })
        }
    }
}

/// PM adapter wrapping [`crate::collect::linear::LinearClient`].
pub struct LinearAdapter {
    inner: crate::collect::linear::LinearClient,
    /// Optional user-supplied detection regex. When `None`, the adapter
    /// falls back to the shared default JIRA-shaped pattern.
    ticket_regex: Option<Regex>,
}

impl LinearAdapter {
    /// Construct from an existing [`crate::collect::linear::LinearClient`]
    /// using the default Linear detection regex.
    pub fn new(inner: crate::collect::linear::LinearClient) -> Self {
        Self {
            inner,
            ticket_regex: None,
        }
    }

    /// Construct from a client and an optional user-supplied detection regex.
    /// See [`JiraAdapter::with_ticket_regex`] for semantics.
    pub fn with_ticket_regex(
        inner: crate::collect::linear::LinearClient,
        pattern: Option<&str>,
    ) -> Self {
        Self {
            inner,
            ticket_regex: compile_user_regex("linear", pattern),
        }
    }
}

#[async_trait]
impl PmAdapter for LinearAdapter {
    fn name(&self) -> &str {
        "linear"
    }

    fn source(&self) -> PmSource {
        PmSource::Linear
    }

    async fn fetch_ticket(&self, ticket_id: &str) -> Result<Option<PmTicket>, PmError> {
        match self.inner.fetch_issue(ticket_id).await {
            Ok(Some(issue)) => {
                let raw = serde_json::to_value(&issue)?;
                Ok(Some(PmTicket {
                    id: issue.identifier,
                    title: issue.title,
                    status: issue.state,
                    ticket_type: String::new(),
                    labels: Vec::new(),
                    url: if issue.url.is_empty() {
                        None
                    } else {
                        Some(issue.url)
                    },
                    source: PmSource::Linear,
                    raw,
                }))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(collect_err_to_pm("linear", e)),
        }
    }

    fn detect_ticket_refs(&self, text: &str) -> Vec<String> {
        // Linear identifiers are a strict subset of the JIRA `KEY-N` shape
        // by default; users can override via `linear.ticket_regex` to
        // accommodate workspace-specific team prefixes.
        match &self.ticket_regex {
            Some(re) => extract_user_regex(re, text),
            None => extract_unique(jira_ref_re(), text),
        }
    }

    async fn health_check(&self) -> Result<(), PmError> {
        // No cheap health endpoint exposed yet — do a no-op fetch.
        match self.inner.fetch_issue("HEALTH-0").await {
            Ok(_) => Ok(()),
            Err(e) => Err(collect_err_to_pm("linear", e)),
        }
    }
}

/// PM adapter wrapping [`crate::collect::azdo::AzureDevOpsClient`].
///
/// Work-item fetching is gated behind ADO Phase 6; until then,
/// `fetch_ticket` returns `Ok(None)`. `health_check` uses the
/// `GET _apis/connectionData` probe that already exists.
pub struct AzureDevOpsAdapter {
    inner: crate::collect::azdo::AzureDevOpsClient,
}

impl AzureDevOpsAdapter {
    /// Construct from an existing [`crate::collect::azdo::AzureDevOpsClient`].
    pub fn new(inner: crate::collect::azdo::AzureDevOpsClient) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl PmAdapter for AzureDevOpsAdapter {
    fn name(&self) -> &str {
        "azure_devops"
    }

    fn source(&self) -> PmSource {
        PmSource::AzureDevOps
    }

    async fn fetch_ticket(&self, ticket_id: &str) -> Result<Option<PmTicket>, PmError> {
        // ADO IDs come in two flavors: bare integers (`123`) or `AB#123`.
        // Strip the `AB#` prefix when present so callers can pass either.
        let numeric = ticket_id.trim_start_matches("AB#");
        let id: u32 = match numeric.parse() {
            Ok(n) => n,
            Err(_) => return Ok(None),
        };

        match self.inner.get_work_items(&[id]).await {
            Ok(items) => Ok(items.into_iter().next().map(|w| {
                let raw = serde_json::json!({
                    "id": w.id,
                    "title": w.title,
                    "state": w.state,
                    "workItemType": w.work_item_type,
                    "tags": w.tags,
                    "teamProject": w.team_project,
                    "url": w.url,
                });
                PmTicket {
                    id: format!("AB#{}", w.id),
                    title: w.title,
                    status: w.state,
                    ticket_type: w.work_item_type,
                    labels: w.tags,
                    url: w.url,
                    source: PmSource::AzureDevOps,
                    raw,
                }
            })),
            // Defensive: any residual NotImplemented variants in the future
            // should degrade gracefully rather than fail the pipeline.
            Err(crate::collect::azdo::AzdoError::NotImplemented { .. }) => Ok(None),
            Err(e) => Err(azdo_err_to_pm(e)),
        }
    }

    fn detect_ticket_refs(&self, text: &str) -> Vec<String> {
        extract_unique(azdo_ref_re(), text)
    }

    async fn health_check(&self) -> Result<(), PmError> {
        match self.inner.test_connection().await {
            Ok(_) => Ok(()),
            Err(e) => Err(azdo_err_to_pm(e)),
        }
    }
}

// ---------------------------------------------------------------------------
// Factory and tests (sub-modules)
// ---------------------------------------------------------------------------

mod factory;
pub use factory::build_adapters;
use factory::{azdo_err_to_pm, collect_err_to_pm};

#[cfg(test)]
mod tests;
