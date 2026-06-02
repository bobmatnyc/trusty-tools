//! Pluggable context-source layer (Phase 6, #550).
//!
//! Why: a review is sharper when the LLM also sees *external* context — the
//! JIRA ticket the PR implements, the Confluence design doc it follows, the
//! GitHub issue it closes.  Rather than hard-wiring each integration into the
//! prompt builder, this module defines a single `ContextSource` trait so every
//! enrichment source (JIRA / Confluence / GitHub Issues today; an indexed APEX
//! knowledgebase in PR-B) plugs into the same orchestrator and renders into the
//! same `## Related <source>` section of the reviewer user message.
//!
//! What: this `mod.rs` is a thin facade.  It defines the shared value types
//! (`ReviewSubject`, `ContextSnippet`, `ContextSection`, `RetrievalMode`,
//! `ContextSourceError`) and the `ContextSource` trait, then re-exports the
//! concrete sources and the orchestrator from sibling modules.
//!
//! ## Fail-open contract (CRITICAL — read before adding a source)
//!
//! These context sources are **supplementary / best-effort enrichment**.  A
//! source that errors, times out, or has no credentials MUST log to stderr and
//! return zero snippets — it MUST NOT block, skip, or fail the review.  This is
//! deliberately DIFFERENT from the trusty-search / trusty-analyze required gate
//! (#590): those two are the *core value* of trusty-review and their absence
//! skips the review; these external sources are nice-to-have and degrade
//! silently to "no extra context".  Every `ContextSource::gather`
//! implementation therefore returns `Result` only so the orchestrator can log
//! the failure — the orchestrator NEVER propagates a source error upward.
//!
//! Test: trait object-safety and the value types are unit-tested in this module;
//! each source carries its own parse/query tests; the orchestrator's fail-open
//! and section-assembly behaviour is tested in `orchestrator`.

pub mod atlassian;
pub mod config;
pub mod confluence;
pub mod github_issues;
pub mod jira;
pub mod orchestrator;

pub use config::{ContextSourcesConfig, ContextSourcesFileConfig, SourceConfig, SourceFileConfig};
pub use confluence::ConfluenceSource;
pub use github_issues::GithubIssuesSource;
pub use jira::JiraSource;
pub use orchestrator::{gather_external_context, render_sections};

use async_trait::async_trait;

// ─── Retrieval mode ─────────────────────────────────────────────────────────

/// How a context source obtains its snippets.
///
/// Why: #550 makes retrieval mode a per-source config knob.  A `Live` source
/// queries its own API at review time; a `Semantic` source queries the
/// trusty-search daemon against a pre-built index (APEX / any knowledgebase).
/// PR-A implements only the `Live` backend for the three external sources; a
/// source configured `Semantic` here returns a clear "not yet implemented"
/// error (logged, fail-open) until PR-B lands the indexed backend.
/// What: a two-variant enum, deserialised lowercase (`"live"` / `"semantic"`).
/// Test: `retrieval_mode_serde_roundtrip`, and the orchestrator's
/// `semantic_mode_not_yet_implemented` path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum RetrievalMode {
    /// Query the source's own API directly at review time (no pre-indexing).
    #[default]
    Live,
    /// Retrieve from a trusty-search index by vector/hybrid search (PR-B).
    Semantic,
}

impl std::fmt::Display for RetrievalMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RetrievalMode::Live => write!(f, "live"),
            RetrievalMode::Semantic => write!(f, "semantic"),
        }
    }
}

// ─── Review subject ─────────────────────────────────────────────────────────

/// The review subject handed to every context source.
///
/// Why: each source searches its backend by the same signal set — the PR title
/// keywords and the identifiers extracted from the diff — so bundling them in
/// one borrow-friendly struct keeps the `ContextSource::gather` signature stable
/// as new sources are added.
/// What: holds the owner/repo (needed by the GitHub-Issues source to scope its
/// search), the PR title, the changed file paths, and the extracted identifiers.
/// All fields are owned so the subject can be cheaply shared across the
/// concurrent gather tasks.
/// Test: constructed by `orchestrator` tests and each source's tests.
#[derive(Debug, Clone, Default)]
pub struct ReviewSubject {
    /// Repository owner / org (GitHub-Issues source scopes its query to this).
    pub owner: String,
    /// Repository name.
    pub repo: String,
    /// PR title (empty in local-diff mode — sources should skip on empty query).
    pub title: String,
    /// Changed file paths from the diff.
    pub changed_files: Vec<String>,
    /// Identifiers extracted from the diff (function/type/symbol names).
    pub identifiers: Vec<String>,
}

impl ReviewSubject {
    /// Build the free-text keyword query a live source should search by.
    ///
    /// Why: every live source searches by the same signal — PR-title words plus
    /// a bounded set of diff identifiers — so the query construction lives here
    /// once instead of being duplicated (and drifting) across three sources.
    /// What: joins the PR title and up to `max_identifiers` identifiers into a
    /// single space-separated string, de-duplicating and dropping empties.
    /// Returns an empty string when there is no usable signal (caller skips).
    /// Test: `keyword_query_combines_title_and_identifiers`,
    /// `keyword_query_empty_when_no_signal` in this module.
    pub fn keyword_query(&self, max_identifiers: usize) -> String {
        let mut parts: Vec<&str> = Vec::new();
        let title = self.title.trim();
        if !title.is_empty() {
            parts.push(title);
        }
        for id in self.identifiers.iter().take(max_identifiers) {
            let id = id.trim();
            if !id.is_empty() && !parts.contains(&id) {
                parts.push(id);
            }
        }
        parts.join(" ")
    }
}

// ─── Snippets + sections ────────────────────────────────────────────────────

/// One retrieved context item from a source.
///
/// Why: sources differ in their native shape (a JIRA ticket, a Confluence page,
/// a GitHub issue), but the prompt only needs a uniform title/subtitle/body/link
/// quad to render a bullet.  Normalising to this struct keeps the section
/// renderer source-agnostic so PR-B's APEX source slots in unchanged.
/// What: a short `title` (such as `PROJ-123 — Add auth`), an optional `subtitle`
/// (status, space, or issue state), an optional `body` snippet, and a `link`.
/// Test: `render_sections_emits_headings_and_bullets` in `orchestrator`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextSnippet {
    /// Primary label for the bullet (ticket key + summary, doc title, etc.).
    pub title: String,
    /// Optional secondary label (status, space, issue state).
    pub subtitle: Option<String>,
    /// Optional body excerpt to embed under the bullet.
    pub body: Option<String>,
    /// Optional canonical link to the item.
    pub link: Option<String>,
}

/// A rendered `## Related <source>` section produced by one source.
///
/// Why: the orchestrator collects one section per source and the prompt builder
/// concatenates them in a deterministic order; carrying the heading alongside
/// the snippets keeps assembly trivial and order-stable.
/// What: `heading` is the markdown H2 text (e.g. `Related JIRA tickets`);
/// `snippets` are the items to render as bullets.  A section with zero snippets
/// is dropped by the orchestrator (nothing to show).
/// Test: `orchestrator` section-ordering and empty-drop tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextSection {
    /// Markdown H2 heading text (without the leading `## `).
    pub heading: String,
    /// Items to render as bullets under the heading.
    pub snippets: Vec<ContextSnippet>,
}

// ─── Error type ─────────────────────────────────────────────────────────────

/// Errors a context source may surface to the orchestrator.
///
/// Why: the orchestrator needs to *log* a meaningful reason when a source fails,
/// even though it never propagates the error (fail-open).  Typed variants let
/// the log line distinguish "missing creds" (skip, expected) from "API error"
/// (transient) from "semantic mode not implemented yet" (config/PR-B).
/// What: a `thiserror` enum (library convention) covering the failure classes
/// every source shares.
/// Test: `context_source_error_display` in this module.
///
/// Note: every variant carries a `src` (the source name) field rather than
/// `source` — the latter name is special-cased by `thiserror` as a `#[source]`
/// error chain and would not satisfy `&'static str: Error`.
#[derive(Debug, thiserror::Error)]
pub enum ContextSourceError {
    /// Required credentials / base URL are absent — the source is simply skipped.
    #[error("{src} skipped: {reason}")]
    NotConfigured {
        /// Source name (e.g. `"jira"`).
        src: &'static str,
        /// Human-readable reason (which env var is missing).
        reason: String,
    },

    /// HTTP transport failure (DNS, connect, TLS, timeout).
    #[error("{src} transport error: {err}")]
    Transport {
        /// Source name.
        src: &'static str,
        /// Underlying transport error text.
        err: TransportErr,
    },

    /// The backend returned a non-2xx status.
    #[error("{src} API returned {status}: {body}")]
    Api {
        /// Source name.
        src: &'static str,
        /// HTTP status code.
        status: u16,
        /// Response body (may be truncated).
        body: String,
    },

    /// The response could not be parsed.
    #[error("{src} response parse error: {detail}")]
    Parse {
        /// Source name.
        src: &'static str,
        /// Parse error text.
        detail: String,
    },

    /// The source is configured `mode = semantic`, which PR-A does not implement.
    #[error("{src}: semantic mode not yet implemented (see PR-B / APEX indexed knowledgebase)")]
    SemanticNotImplemented {
        /// Source name.
        src: &'static str,
    },
}

/// Newtype wrapper carrying a transport error message.
///
/// Why: reqwest errors are not `Clone` and we want a simple owned string for
/// testability; a newtype keeps `Transport` self-describing.
/// What: wraps a message string with a `Display` impl.
/// Test: covered transitively by `context_source_error_display`.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct TransportErr(pub String);

// ─── The trait ──────────────────────────────────────────────────────────────

/// A pluggable external context source.
///
/// Why: this is the seam that lets the orchestrator treat JIRA, Confluence,
/// GitHub Issues, and (PR-B) an indexed APEX knowledgebase identically.  Each
/// source declares its name + enabled flag + retrieval mode and knows how to
/// turn a `ReviewSubject` into a `ContextSection`.
/// What: `name` is a stable identifier used in logs and the config table;
/// `is_enabled` lets the orchestrator skip a disabled source cheaply (without a
/// network round-trip); `mode` is the configured retrieval mode; `gather`
/// performs the actual retrieval.
///
/// Fail-open contract: `gather` returns `Result` ONLY so the orchestrator can
/// log a reason; the orchestrator treats `Err(_)` exactly like an empty section
/// and continues.  Implementations should therefore NOT panic and should map
/// every failure to a `ContextSourceError`.
/// Test: `context_source_object_safe` (object-safety) in this module; per-source
/// behaviour in each source's own tests.
#[async_trait]
pub trait ContextSource: Send + Sync {
    /// Stable source identifier (e.g. `"jira"`), used in logs + config keys.
    fn name(&self) -> &'static str;

    /// Whether this source is enabled (config + credentials present).
    fn is_enabled(&self) -> bool;

    /// The configured retrieval mode for this source.
    fn mode(&self) -> RetrievalMode;

    /// Retrieve context for the review subject.
    ///
    /// Returns a `ContextSection` (possibly empty) on success.  On any failure
    /// returns a `ContextSourceError` for the orchestrator to log; the
    /// orchestrator NEVER propagates this error (fail-open).
    async fn gather(&self, subject: &ReviewSubject) -> Result<ContextSection, ContextSourceError>;
}

// ─── Unit tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retrieval_mode_serde_roundtrip() {
        let live: RetrievalMode = serde_json::from_str("\"live\"").unwrap();
        assert_eq!(live, RetrievalMode::Live);
        let sem: RetrievalMode = serde_json::from_str("\"semantic\"").unwrap();
        assert_eq!(sem, RetrievalMode::Semantic);
        assert_eq!(RetrievalMode::default(), RetrievalMode::Live);
        assert_eq!(
            serde_json::to_string(&RetrievalMode::Semantic).unwrap(),
            "\"semantic\""
        );
    }

    #[test]
    fn keyword_query_combines_title_and_identifiers() {
        let subj = ReviewSubject {
            title: "Add auth flow".to_string(),
            identifiers: vec!["authenticate".to_string(), "TokenStore".to_string()],
            ..Default::default()
        };
        let q = subj.keyword_query(8);
        assert_eq!(q, "Add auth flow authenticate TokenStore");
    }

    #[test]
    fn keyword_query_dedupes_and_caps_identifiers() {
        let subj = ReviewSubject {
            title: "fix".to_string(),
            identifiers: vec![
                "fix".to_string(), // duplicate of title token — dropped
                "a".to_string(),
                "b".to_string(),
                "c".to_string(),
            ],
            ..Default::default()
        };
        // The cap of 2 applies to the identifier iterator BEFORE dedup, so we
        // consider ["fix", "a"]; "fix" duplicates the title token and is dropped,
        // leaving just "a". ("b"/"c" are past the cap.)
        let q = subj.keyword_query(2);
        assert_eq!(q, "fix a");
    }

    #[test]
    fn keyword_query_empty_when_no_signal() {
        let subj = ReviewSubject {
            title: "   ".to_string(),
            identifiers: vec![String::new(), "  ".to_string()],
            ..Default::default()
        };
        assert_eq!(subj.keyword_query(8), "");
    }

    #[test]
    fn context_source_error_display() {
        let e = ContextSourceError::NotConfigured {
            src: "jira",
            reason: "ATLASSIAN_API_TOKEN unset".to_string(),
        };
        assert!(e.to_string().contains("jira skipped"));
        assert!(e.to_string().contains("ATLASSIAN_API_TOKEN"));

        let e = ContextSourceError::SemanticNotImplemented { src: "jira" };
        assert!(e.to_string().contains("semantic mode not yet implemented"));
        assert!(e.to_string().contains("PR-B"));

        let e = ContextSourceError::Api {
            src: "confluence",
            status: 503,
            body: "overloaded".to_string(),
        };
        let s = e.to_string();
        assert!(s.contains("confluence"));
        assert!(s.contains("503"));
    }

    #[test]
    fn context_source_object_safe() {
        // Proves `ContextSource` is object-safe (usable as `dyn`).
        fn _accepts(_s: &dyn ContextSource) {}
    }
}
