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

pub mod absence_claim;
// Why: keeps the raw structured reviewer payload out of `review_body`, which
// consumers read (and post) as the human-readable review (#4999).
pub(crate) mod body_render;
pub mod citation_check;
// Why: the grounding guard for package-registry / version-existence claims
// (#4081) — kept separate from `finding_hygiene` (self-admission markers) and
// `citation_check` (path/content verification) because it keys on a different
// signal entirely and carries its own marker sets and test surface.
pub mod claim_grounding;
pub mod context_gate;
pub mod diff;
pub mod diff_analyzer;
// Why: the general form of #4081's rule — a claim may be marked `confirmed`
// only when something actually checked it — keyed on the finding's own
// epistemic admission rather than on `claim_grounding`'s registry vocabulary,
// which is what let the same defect recur on an in-repo signature claim (#5309).
pub mod evidence_admission;
// Why: output-hygiene filters (self-negation / chain-of-thought-leak drop,
// diff-absent-speculation demotion) closing #4043/#4044 — kept separate from
// `citation_check` (path/content verification, #4042) since the two concerns
// have independent marker sets and independent test surfaces.
pub mod finding_hygiene;
pub mod grade;
// Why: reconciles the grade embedded in the raw `review_body` JSON with the
// authoritative top-level grade so the two can never disagree (issue #1886).
pub mod grade_reconcile;
pub mod letter_grade;
pub mod mapreduce;
pub mod output;
pub mod parser;
pub mod post;
pub mod prompt;
pub mod runner;
// Why: the map-reduce branch of `run_review` (split → map → reduce → fold) is
// extracted here to keep runner.rs under the 500-line cap (#610 / #1643).
pub mod runner_context;
pub mod runner_mapreduce;
pub mod trigger;
pub mod verify;
pub mod verify_liveness;
pub mod verify_prompt;
// Why: the narrative summary is written before the verification round on both
// pipeline paths and nothing revisits it, so a refuted finding kept being cited
// as a merge blocker (#4044).  Kept separate from `grade_reconcile` (which
// rewrites an embedded JSON grade) — different signal, different test surface.
pub mod verification_notice;
// Why: voice-config resolution is extracted from runner.rs to keep runner.rs
// under the 500-line cap (#610).  Exposes `build_voice_config` for use by the
// runner and for direct testing.
pub mod voice_config;
// Why: coverage data loading extracted from runner.rs to keep that file under
// the 500-line cap (#610) after adding coverage-gating pipeline (#1014).
pub mod runner_coverage;
// Why: helper functions (apply_grade_and_floor, fetch_github_pr_meta, abort_dry,
// finalize_run) extracted from runner.rs to keep it under the 500-line cap (#610).
pub mod runner_helpers;
// Why: system-prompt string constants extracted from prompt.rs to keep it under
// the 500-line cap (#610) after the two coverage-gating variants were added (#1014).
pub mod prompt_templates;
// Why: user-message builder extracted from prompt.rs to keep it under the
// 500-line cap (#610) — the function is 145 lines on its own.
pub mod prompt_user_msg;

pub use context_gate::{GateOutcome, degraded_banner, preflight_context};
pub use diff::DiffSource;
pub use grade::{derive_verdict, derive_verdict_with_grade};
pub use letter_grade::{
    Grade, clamp_grade_to_verdict, default_grade_for_verdict, verdict_for_grade,
};
pub use output::{log_json_path, print_review_result, write_review_log};
pub use parser::{ParsedReview, parse_review_response};
pub use post::{
    DryRunReason, FinalizeAction, PostContext, decide_action, finalize_review, surface_dry_run,
};
pub use prompt::{
    ReviewContext, ReviewPrMeta, build_review_prompt, build_review_prompt_with_coverage,
    build_system_prompt, build_system_prompt_with_coverage, reviewer_system_prompt,
    reviewer_system_prompt_with_coverage,
};
pub use runner::{CallerContext, ReviewDeps, ReviewInput, run_review};
pub use trigger::{TriggerDecision, classify_review_request, effective_dry_run};
pub use verify::{maybe_verify, run_verification_round, select_candidates};
pub use verify_liveness::{LivenessDecision, enforce_verifier_liveness, probe_verifier_liveness};
pub use voice_config::build_voice_config;
