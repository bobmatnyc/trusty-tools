//! AUDIT mode — the one-shot, full-dataset sweep behind `tga audit` (DOC-67).
//!
//! Why: acquisition due diligence points one command at an org and reads the
//! result once, under time pressure. DOC-67 §2 makes that literal — nothing in
//! this module prompts, waits for input, or needs a terminal.
//! What: [`run_full_sweep`], the library entry point that drives tga's eight
//! data-collection subcommands end to end — plus the commit ↔ board-item
//! correlation pass (#5405) — and the per-stage outcome types it returns. The `tga audit` command (`crate::commands::audit`) owns
//! orchestration and reporting; this module owns stage sequencing and nothing
//! else.
//! Test: `audit::tests`.
//!
//! ## Stage order
//!
//! DOC-67 §5's prose lists the sweep as "collect → classify → report →
//! pr-metrics → jira → dora → deployments → incidents". That is the spec's
//! enumeration of which subcommands participate, not a runnable order: `dora`
//! reduces `fact_deployments` / `fact_incidents` (§8), so running it before the
//! two commands that populate those tables would compute the four DORA keys
//! over empty input, and `report` renders what the earlier stages produced, so
//! running it third would render a mostly empty report. The executed order is
//! therefore data-flow order — see [`SweepStage`].

mod analyze;
mod gaps;
mod repo_index;
mod review;
mod search_daemon;
mod stage;
mod sweep;

#[cfg(test)]
mod tests;

#[cfg(all(test, unix))]
mod real_binary_tests;

pub use analyze::{
    ensure_analyze_daemon, ensure_analyze_daemon_with, AnalyzeDaemonUnavailable, AnalyzeGuard,
    DEFAULT_ANALYZE_BIN, DEFAULT_ANALYZE_PORT, DEFAULT_ANALYZE_URL, ENV_ANALYZE_BIN,
    ENV_ANALYZE_URL,
};
/// The excerpt cap, visible to `report::dd_manifest_tests` so its
/// boundary-straddle tests position a credential against the real value instead
/// of a copy that can drift away from it (#5308 review).
#[cfg(test)]
pub(crate) use gaps::MAX_REASON_CHARS;
pub use gaps::{index_gap_lines, sweep_gap_lines, DATA_HANDLING_NOTE};
pub use repo_index::{
    ensure_repositories_indexed, index_id_for, resolve_search_binary, RepoIndexOutcome,
    RepoIndexStatus, DEFAULT_SEARCH_BIN, ENV_SEARCH_BIN,
};
pub use review::{
    artifact_paths, require_inference_credential, require_rendered_report_carries_synthesis,
    require_review_supports_required_inference, resolve_review_binary, run_review_report,
    MissingInferenceCredential, ReviewBinaryTooOld, ReviewRun, ReviewRunError, UnverifiedReport,
    DEFAULT_REVIEW_BIN, ENV_INFERENCE_CREDENTIAL, ENV_REVIEW_BIN, MIN_REVIEW_VERSION,
};
/// #5670: link 1 of the prerequisite chain — the daemon `trusty-analyze` itself
/// refuses to boot without, and reports `503 degraded` for as long as it is gone.
pub use search_daemon::{
    ensure_search_daemon, ensure_search_daemon_with, SearchDaemonUnavailable, SearchGuard,
    SEARCH_STARTUP_TIMEOUT,
};
pub use stage::{AuditSweepStats, StageOutcome, StageStatus, StaleFetch, SweepStage};
pub use sweep::{run_full_sweep, SweepOptions};
