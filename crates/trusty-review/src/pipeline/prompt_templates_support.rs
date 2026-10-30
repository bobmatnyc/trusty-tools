//! Test-only sources for the reviewer prompts (#9015).
//!
//! Why: the prompts left the crate directory, so a unit test that builds a
//! review reads the checkout's `content/`. The staged source and the
//! per-thread override live in `crate::runtime_content::test_support`, which
//! every content reader shares; this module keeps the prompt-shaped helpers.
//! What: [`prompts`] reads both prompts from the staged content;
//! [`repo_prompt_files`] lists the checkout's prompt files, the principles
//! addendum included (#9991).
//! Test: `the_repository_prompts_load_byte_identical`, and the pipeline arm
//! `run_review_without_prompts_fails_closed_before_any_fetch_or_model_call`.

use std::sync::OnceLock;

pub(crate) use crate::runtime_content::test_support::{OverrideGuard, fake_checkout};

use super::{COVERAGE_GATING_FILE, PROMPTS_DIR, ReviewerPrompts, STOCK_FILE};
use crate::voice::principles::PRINCIPLES_FILE;

/// The checkout's two prompt files and the principles addendum (#9991) as
/// `(repo-relative path, bytes)`, read from the runtime checkout (#9298), never
/// the compile-time build path.
pub(crate) fn repo_prompt_files() -> Vec<(String, Vec<u8>)> {
    let root = trusty_common::test_harness::test_repo_root()
        .expect("resolve the trusty-tools checkout (set TRUSTY_TEST_REPO_ROOT)");
    [STOCK_FILE, COVERAGE_GATING_FILE, PRINCIPLES_FILE]
        .iter()
        .map(|file| {
            let rel = format!("content/{PROMPTS_DIR}/{file}");
            let bytes = std::fs::read(root.join(&rel)).expect("read a checkout prompt");
            (rel, bytes)
        })
        .collect()
}

/// The checkout's prompts, read through the resolver from the staged content;
/// loaded once per test process.
pub(crate) fn prompts() -> &'static ReviewerPrompts {
    static PROMPTS: OnceLock<ReviewerPrompts> = OnceLock::new();
    PROMPTS.get_or_init(|| {
        let content = crate::runtime_content::test_support::staged();
        ReviewerPrompts::from_content(&content).expect("the checkout's prompts load")
    })
}
