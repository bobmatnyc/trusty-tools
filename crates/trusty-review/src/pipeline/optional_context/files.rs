//! The PR's changed files, read whole at the head SHA (#9195).
//!
//! STUB: the call shape only; the reads land in the next commit.

use std::sync::Arc;

use crate::{
    config::{MapReduceConfig, ReviewPath},
    integrations::context::contents_at_ref::DocFetcher,
    pipeline::{diff::DiffSource, diff_analyzer::models::FilteredDiff},
};

use super::{
    OptionalContextRequest, ReviewOptions, assemble::AppliedContext, files_select::Candidate,
    ledger::ContextLedger, seams::PrHead,
};

/// Everything [`apply_files`] reads from the runner (#9195).
pub(crate) struct FilesCall<'a> {
    request: &'a OptionalContextRequest,
    seam: Option<Arc<dyn DocFetcher>>,
    source: &'a DiffSource,
    candidates: Vec<Candidate>,
    head: PrHead,
}

impl<'a> FilesCall<'a> {
    /// What the runner knows before the head is attached.
    pub(crate) fn new(
        options: &'a ReviewOptions,
        source: &'a DiffSource,
        _filtered: &FilteredDiff,
        _raw_diff: &str,
        _path: (ReviewPath, &MapReduceConfig),
    ) -> Self {
        Self {
            request: &options.request,
            seam: options.doc_fetcher.clone(),
            source,
            candidates: Vec::new(),
            head: PrHead::default(),
        }
    }

    /// This call reading at `head`.
    pub(crate) fn at(mut self, head: &PrHead) -> Self {
        self.head = head.clone();
        self
    }
}

/// Read the changed files at the head and leave them in `applied` (#9195).
pub(crate) async fn apply_files(
    _applied: &mut AppliedContext,
    call: FilesCall<'_>,
    _ledger: &mut ContextLedger,
) {
    let _ = (
        call.request,
        call.seam,
        call.source,
        call.candidates,
        call.head,
    );
}

#[cfg(test)]
#[path = "files_tests.rs"]
mod tests;
