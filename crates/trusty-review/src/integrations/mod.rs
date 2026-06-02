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
//!
//! Deferred to later stages: `jira`, `slack`.
//!
//! Test: each submodule carries its own unit tests.

pub mod analyze_client;
pub mod github;
pub mod search_client;

pub use analyze_client::{
    AnalyzeClient, AnalyzeClientError, AnalyzeHealthResponse, AnalyzeIndexInfo, ComplexityHotspot,
    HttpAnalyzeClient, Smell,
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
