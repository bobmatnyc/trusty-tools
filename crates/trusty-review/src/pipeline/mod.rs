//! Review pipeline — diff loading, context retrieval, LLM review, parsing,
//! and output.
//!
//! Why: groups the pipeline stages into focused submodules so each can
//! be tested and iterated independently without touching the others.
//! What: re-exports the public items needed by the CLI (`run_review`,
//! `ReviewInput`, `ReviewDeps`, `DiffSource`) and the compare aggregator.
//!
//! Submodules:
//!  - `diff`    — diff source, loading, truncation, identifier extraction.
//!  - `grade`   — severity-anchored deterministic grade derivation (floor logic).
//!  - `prompt`  — prompt construction for the reviewer role.
//!  - `parser`  — verdict + findings parsing from LLM responses.
//!  - `output`  — log file writing and STDOUT rendering.
//!  - `runner`  — top-level orchestration loop (`run_review`).
//!
//! Test: each submodule carries its own unit tests.

pub mod diff;
pub mod grade;
pub mod output;
pub mod parser;
pub mod prompt;
pub mod runner;

pub use diff::DiffSource;
pub use grade::derive_verdict;
pub use output::{log_json_path, print_review_result, write_review_log};
pub use parser::{ParsedReview, parse_review_response};
pub use prompt::{ReviewContext, ReviewPrMeta, build_review_prompt, reviewer_system_prompt};
pub use runner::{ReviewDeps, ReviewInput, run_review};
