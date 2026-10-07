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

use crate::models::{ContextSourceRecord, ReviewResult};

pub(crate) mod assemble;
pub(crate) mod ledger;
pub(crate) mod seams;

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
/// MCP `review_pr` text params). `report_context` asks for the source ledger
/// with no other new input.
/// Test: `include_pr_body_reaches_reviewer_and_verifier_prompts`,
/// `requested_new_is_off_by_default_and_on_with_pr_body`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct OptionalContextRequest {
    /// Merge the fetched PR body into the reviewer's PR description.
    pub include_pr_body: bool,
    /// Caller text arrived through a new parameter (MCP `review_pr`).
    pub caller_text: bool,
    /// Report the source ledger even with no other new input.
    pub report_context: bool,
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

    /// Whether any new input is on.
    ///
    /// Why: plan §3.1 (Architect ruling 2026-10-06 03:42Z): any new
    /// parameter counts. The legacy `run` text flags and a `# Context:`
    /// preamble never set `caller_text`, so they never count.
    /// Test: `requested_new_is_off_by_default_and_on_with_pr_body`,
    /// `stdin_context_and_pr_description_never_report`.
    pub fn requested_new(&self) -> bool {
        self.include_pr_body || self.caller_text
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
/// What: the caller's [`OptionalContextRequest`], plus the PR seam tests
/// inject; production leaves the seam `None`.
/// Test: `off_is_byte_identical_unified`,
/// `include_pr_body_reaches_reviewer_and_verifier_prompts`.
#[derive(Clone, Default)]
#[non_exhaustive]
pub struct ReviewOptions {
    /// The optional inputs this review asked for.
    pub request: OptionalContextRequest,
    /// Test seam for the GitHub metadata and diff reads (#9192).
    pub(crate) pr_source: Option<Arc<dyn PrSource>>,
}

impl ReviewOptions {
    /// Options carrying `request` and the production PR reads.
    pub fn new(request: OptionalContextRequest) -> Self {
        Self {
            request,
            pr_source: None,
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
