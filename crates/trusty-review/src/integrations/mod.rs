//! External integration clients for trusty-review.
//!
//! Why: all network-facing adapters live in this module so the rest of the
//! pipeline depends on trait boundaries, not concrete transport types.
//! (spec REV-009, doc 05-integrations)
//!
//! What: sub-modules:
//!   - `github` — GitHub App auth, PR diff/metadata fetch, push firewall,
//!     webhook HMAC verification.
//!   - `search_client` — HTTP client over trusty-search `:7878` (REQUIRED).
//!   - `analyze_client` — HTTP client over trusty-analyze `:7879` (OPTIONAL).
//!   - `context` — pluggable external context sources (JIRA / Confluence /
//!     GitHub Issues today; APEX/knowledgebase in PR-B).  Best-effort / fail-open
//!     enrichment, distinct from the REQUIRED search/analyze gate (#550, #590).
//!
//! Deferred to later stages: `slack`.
//!
//! Test: each submodule carries its own unit tests.

pub mod analyze_client;
pub mod apex_context;
pub mod context;
pub mod github;
pub mod search_client;

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
pub use search_client::{
    HealthResponse, HttpSearchClient, IndexInfo, SearchClient, SearchClientError, SearchRequest,
    SearchResponse, SearchResult,
};
