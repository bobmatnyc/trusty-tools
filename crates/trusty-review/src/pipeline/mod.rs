//! Review pipeline — diff loading, context retrieval, LLM review, parsing,
//! and output.
//!
//! Why: groups the pipeline stages into focused submodules so each can
//! be tested and iterated independently without touching the others.
//! What: re-exports the public items needed by the CLI (`run_review`,
//! `ReviewInput`, `ReviewDeps`, `DiffSource`) and the compare aggregator.
//!
//! Submodules:
//!  - `diff`          — diff source, loading, truncation, identifier extraction.
//!  - `diff_analyzer` — DiffAnalyzer noise filter: Stages A/B/C (spec REV-200–262).
//!  - `grade`         — severity-anchored deterministic grade derivation (floor logic).
//!  - `letter_grade`  — letter-grade type (A+ through F) and grade→verdict mapping (#732).
//!  - `mapreduce`     — per-file diff splitter + (future) map/reduce stages (#680).
//!  - `prompt`        — prompt construction for the reviewer role.
//!  - `parser`        — verdict + findings parsing from LLM responses.
//!  - `output`        — log file writing and STDOUT rendering.
//!  - `post`          — post-or-log finalisation decision (Phase 1, #582).
//!  - `verify_prompt` — verifier-pass prompt + forced-output schema (Phase 2, #583).
//!  - `verify`        — per-finding verification round + liveness gate (Phase 2, #583).
//!  - `runner`        — top-level orchestration loop (`run_review`).
//!  - `trigger`       — live vs dry-run trigger classification (REV-703).
//!  - `voice_config`  — VoiceConfig resolution (stock, principles, voice) from ReviewConfig (#754, #756).
//!
//! Test: each submodule carries its own unit tests.

pub mod context_gate;
pub mod diff;
pub mod diff_analyzer;
pub mod grade;
pub mod letter_grade;
pub mod mapreduce;
pub mod output;
pub mod parser;
pub mod post;
pub mod prompt;
pub mod runner;
pub mod runner_context;
pub mod trigger;
pub mod verify;
pub mod verify_liveness;
pub mod verify_prompt;
// Why: voice-config resolution is extracted from runner.rs to keep runner.rs
// under the 500-line cap (#610).  Exposes `build_voice_config` for use by the
// runner and for direct testing.
pub mod voice_config;

pub use context_gate::{GateOutcome, degraded_banner, preflight_context};
pub use diff::DiffSource;
pub use grade::{derive_verdict, derive_verdict_with_grade};
pub use letter_grade::{
    Grade, clamp_grade_to_verdict, default_grade_for_verdict, verdict_for_grade,
};
pub use output::{log_json_path, print_review_result, write_review_log};
pub use parser::{ParsedReview, parse_review_response};
pub use post::{FinalizeAction, PostContext, decide_action, finalize_review};
pub use prompt::{
    ReviewContext, ReviewPrMeta, build_review_prompt, build_system_prompt, reviewer_system_prompt,
};
pub use runner::{ReviewDeps, ReviewInput, run_review};
pub use trigger::{TriggerDecision, classify_review_request, effective_dry_run};
pub use verify::{maybe_verify, run_verification_round, select_candidates};
pub use verify_liveness::{LivenessDecision, enforce_verifier_liveness, probe_verifier_liveness};
pub use voice_config::build_voice_config;
