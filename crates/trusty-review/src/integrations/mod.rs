//! External integration clients for trusty-review.
//!
//! Why: all network-facing adapters live in this module so the rest of the
//! pipeline depends on trait boundaries, not concrete transport types.
//! (spec REV-009, doc 05-integrations)
//!
//! What: sub-modules:
//!   - `github` — GitHub App auth, PR diff/metadata fetch, push firewall,
//!     webhook HMAC verification.
//!   - `health` — tolerant `HealthResponse` / `EmbedderState` types for the
//!     trusty-search `/health` wire format (accepts both bool and string forms;
//!     closes #628).
//!   - `search_client` — HTTP client over trusty-search `:7878` (REQUIRED).
//!   - `null_search_client` — no-op `SearchClient` used by the `--source-root`
//!     diff-only fallback when no registered index matches (#2994).
//!   - `analyze_client` — HTTP client over trusty-analyze `:7879` (OPTIONAL).
//!   - `null_analyze_client` — no-op `AnalyzeClient` used alongside
//!     `null_search_client` by the same `--source-root` diff-only fallback
//!     (#2994).
//!   - `context` — pluggable external context sources (JIRA / Confluence /
//!     GitHub Issues today; APEX/knowledgebase in PR-B).  Best-effort / fail-open
//!     enrichment, distinct from the REQUIRED search/analyze gate (#550, #590).
//!   - `subprocess_analyze_client` — on-demand `AnalyzeClient` that spawns
//!     `trusty-analyze` as a subprocess instead of calling a running daemon.
//!     (closes #632)
//!
//! Deferred to later stages: `slack`.
//!
//! Test: each submodule carries its own unit tests.

pub mod analyze_client;
pub mod apex_context;
pub mod context;
pub mod github;
pub mod health;
pub mod null_analyze_client;
pub mod null_search_client;
pub mod search_client;
pub mod subprocess_analyze_client;

pub use analyze_client::{
    AnalyzeClient, AnalyzeClientError, AnalyzeHealthResponse, AnalyzeIndexInfo, ComplexityHotspot,
    HttpAnalyzeClient, Smell,
};
pub use apex_context::{ApexContextResult, fetch_apex_context};
pub use context::{
    ConfluenceSource, ContextSection, ContextSnippet, ContextSource, ContextSourceError,
    ContextSourcesConfig, ContextSourcesFileConfig, GithubIssuesSource, JiraSource, RetrievalMode,
    ReviewSubject, SourceConfig, gather_external_context, render_sections,
};
pub use github::{
    AuthStrategy, GH_ALLOW_PUSH, GithubClient, GithubError, PostedReview, PrMetadata, PrRef,
    PrUser, RunMode, assert_no_push_operation, fetch_pr_diff, fetch_pr_metadata, mint_app_jwt,
    post_pr_review, resolve_token_for_mode, verify_webhook_signature,
};
pub use null_analyze_client::NullAnalyzeClient;
pub use null_search_client::NullSearchClient;
pub use search_client::{
    EmbedderState, HealthResponse, HttpSearchClient, IndexInfo, SearchClient, SearchClientError,
    SearchRequest, SearchResponse, SearchResult,
};
pub use subprocess_analyze_client::SubprocessAnalyzeClient;
