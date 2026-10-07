//! Optional review inputs and the `run_review_with` entry point's types (#9192).
//!
//! Why: epic #9191 adds inputs a caller must ask for, and the owner ruled the
//! library stays source-compatible: `CallerContext`, `ReviewDeps` and
//! `ReviewResult` keep their shape, so the new inputs travel beside them.
//! What: [`OptionalContextRequest`] names the inputs a caller asked for;
//! [`ReviewOptions`] carries it into `run_review_with`; [`ReviewOutcome`] is
//! what that returns, with the ledger of sources the request used.
//! Test: `off_is_byte_identical_unified`, `off_is_byte_identical_mapreduce`,
//! `include_pr_body_reaches_reviewer_and_verifier_prompts`.

use std::sync::Arc;

use crate::integrations::context::{ContextSource, contents_at_ref::DocFetcher};
use crate::models::{ContextSourceRecord, ReviewResult};

pub(crate) mod assemble;
pub(crate) mod doc_refs; // #9193
pub(crate) mod docs; // #9193
pub(crate) mod docs_render; // #9193
pub(crate) mod issues;
pub(crate) mod ledger;
pub(crate) mod probes; // #9194
pub(crate) mod seams;

pub use issues::{IssueDoc, IssueDocsError}; // #9197
pub(crate) use seams::PrSource;

/// The optional inputs a caller asked a review for (#9192).
///
/// Why: each input is off unless asked for, so a default request reviews
/// exactly as `run_review` does.
/// What: `include_pr_body` merges the fetched PR body, capped at
/// `MAX_PR_BODY_CHARS` with its own marker and fenced as data, into the
/// reviewer's PR description, ahead of any caller text. A local diff has no
/// PR body; the ledger records it `unavailable` and the review runs.
/// `caller_text` marks caller text that arrived through a new parameter (the
/// MCP `review_pr` text params). `issue_docs` (#9197) are caller issue docs
/// for the reviewer only, `Some` whenever the caller sent the parameter, even
/// an empty list. `spec_docs` and `claude_md` (#9193) read ADR/spec/SLD docs
/// and CLAUDE.md files at the PR head SHA, for the reviewer only.
/// `report_context` asks for the source ledger with no other new input.
/// Test: `include_pr_body_reaches_reviewer_and_verifier_prompts`,
/// `requested_new_is_off_by_default_and_on_with_pr_body`,
/// `issue_docs_turn_the_ledger_on`, `doc_flags_turn_the_ledger_on`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct OptionalContextRequest {
    /// Merge the fetched PR body into the reviewer's PR description.
    pub include_pr_body: bool,
    /// Caller text arrived through a new parameter (MCP `review_pr`).
    pub caller_text: bool,
    /// Report the source ledger even with no other new input.
    pub report_context: bool,
    /// Caller issue docs for the reviewer (#9197); `None` when not sent.
    pub issue_docs: Option<Vec<IssueDoc>>,
    /// Read the docs the PR body names, plus search hits, at the head (#9193).
    pub spec_docs: bool,
    /// Read CLAUDE.md conventions at the head (#9193).
    pub claude_md: bool,
}

impl OptionalContextRequest {
    /// This request with `include_pr_body` set to `on`.
    #[must_use]
    pub fn with_pr_body(mut self, on: bool) -> Self {
        self.include_pr_body = on;
        self
    }

    /// This request with `caller_text` set to `on`.
    #[must_use]
    pub fn with_caller_text(mut self, on: bool) -> Self {
        self.caller_text = on;
        self
    }

    /// This request with `report_context` set to `on`.
    #[must_use]
    pub fn with_report_context(mut self, on: bool) -> Self {
        self.report_context = on;
        self
    }

    /// This request carrying the caller's issue docs (#9197).
    ///
    /// Why: Architect ruling 2026-10-06 04:47Z: issue docs enter through the
    /// request, so no public review type gains a field.
    /// What: sets `issue_docs`, which counts as a new input even when empty.
    /// Test: `issue_docs_turn_the_ledger_on`.
    #[must_use]
    pub fn with_issue_docs(mut self, docs: Vec<IssueDoc>) -> Self {
        self.issue_docs = Some(docs);
        self
    }

    /// This request with `spec_docs` set to `on` (#9193).
    #[must_use]
    pub fn with_spec_docs(mut self, on: bool) -> Self {
        self.spec_docs = on;
        self
    }

    /// This request with `claude_md` set to `on` (#9193).
    #[must_use]
    pub fn with_claude_md(mut self, on: bool) -> Self {
        self.claude_md = on;
        self
    }

    /// Whether any new input is on.
    ///
    /// Why: plan §3.1 (Architect ruling 2026-10-06 03:42Z): any new
    /// parameter counts. The legacy `run` text flags and a `# Context:`
    /// preamble never set `caller_text`, so they never count.
    /// Test: `requested_new_is_off_by_default_and_on_with_pr_body`,
    /// `stdin_context_and_pr_description_never_report`,
    /// `issue_docs_turn_the_ledger_on`, `doc_flags_turn_the_ledger_on`.
    pub fn requested_new(&self) -> bool {
        // #9197: sending `issue_docs` is a new input; #9193: so is either doc flag.
        self.include_pr_body
            || self.caller_text
            || self.issue_docs.is_some()
            || self.spec_docs
            || self.claude_md
    }

    /// Whether the review keeps a source ledger: a new input, or a request
    /// for the report itself.
    ///
    /// Test: `report_context_alone_turns_the_ledger_on`.
    pub fn ledger_enabled(&self) -> bool {
        self.requested_new() || self.report_context
    }
}

/// What `run_review_with` takes beyond `run_review`'s arguments (#9192).
///
/// Why: a default value runs the review exactly as `run_review` does, so a
/// caller opts in to each new input and nothing else changes.
/// What: the caller's [`OptionalContextRequest`], plus the PR and doc-read
/// seams tests inject; production leaves both `None`.
/// Test: `off_is_byte_identical_unified`,
/// `include_pr_body_reaches_reviewer_and_verifier_prompts`.
#[derive(Clone, Default)]
#[non_exhaustive]
pub struct ReviewOptions {
    /// The optional inputs this review asked for.
    pub request: OptionalContextRequest,
    /// Test seam for the GitHub metadata and diff reads (#9192).
    pub(crate) pr_source: Option<Arc<dyn PrSource>>,
    /// Test seam for the Contents API doc reads (#9193).
    pub(crate) doc_fetcher: Option<Arc<dyn DocFetcher>>,
    /// Test seam for the external context sources (#9194).
    pub(crate) external_sources: Option<ExternalSources>,
}

/// Builds the external context sources a review gathers from (#9194 seam).
pub(crate) type ExternalSources = Arc<dyn Fn() -> Vec<Box<dyn ContextSource>> + Send + Sync>;

impl ReviewOptions {
    /// Options carrying `request` and the production PR reads.
    pub fn new(request: OptionalContextRequest) -> Self {
        Self {
            request,
            pr_source: None,
            doc_fetcher: None,
            external_sources: None,
        }
    }
}

/// What `run_review_with` returns (#9192).
///
/// Why: the review result keeps its serialized shape; the source ledger
/// travels beside it, and the MCP envelope reports it as `context_sources`.
/// What: the `ReviewResult` `run_review` would have returned, and one record
/// per optional source the request turned on. `context_sources` is empty
/// unless [`OptionalContextRequest::ledger_enabled`] holds, and empty on a
/// review that ended before its context was gathered.
/// Test: `context_sources_absent_unless_requested`,
/// `a_new_input_turns_the_ledger_on`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ReviewOutcome {
    /// The review result, identical in shape to what `run_review` returns.
    pub result: ReviewResult,
    /// The optional sources this review used, and how.
    pub context_sources: Vec<ContextSourceRecord>,
}
