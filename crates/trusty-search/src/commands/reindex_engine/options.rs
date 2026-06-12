//! Reindex option and outcome record types.
//!
//! Why: callers such as `run_reindex_opts` and `run_reindex_force_opts` need to
//! pass different combinations of options without a growing argument list, and
//! a single return type captures everything the post-verify step and summary
//! line need.
//! What: `ReindexOptions` (input knobs) and `ReindexOutcome` (SSE `complete`
//! record), both plain `Copy` structs.
//! Test: `tests::default_options_are_sane` / `tests::default_outcome_is_zero`.

use crate::commands::reindex_ui::ReindexTimings;

/// Options controlling reindex CLI behaviour.
///
/// Why: callers such as `run_reindex_opts` and `run_reindex_force_opts` need to
/// pass different combinations of options without a growing argument list.
/// What: a plain struct with `Default` so callers can specify only the fields
/// they care about.
/// Test: default values are asserted by `tests::default_options_are_sane`.
#[derive(Debug, Clone, Copy)]
pub struct ReindexOptions {
    /// After the reindex completes, fetch `/status` and issue a sanity-check
    /// search to verify the index is healthy. Enabled by `--force` to give
    /// the user a blue-green-style safety net.
    ///
    /// Note: the daemon's reindex is NOT atomic blue-green — it mutates the
    /// in-memory index in place via a write lock per batch (see
    /// `crates/trusty-search-service/src/reindex.rs::spawn_reindex` —
    /// `index_files_batch_no_rebuild` adds chunks per-batch). If verify fails
    /// after a `--force`, the index is already in its new (possibly broken)
    /// state. We surface that fact loudly so the user can manually re-run.
    pub verify_after: bool,
    /// Chunk count snapshot taken before the reindex started, used to print
    /// "(was N)" in the final verify message.
    pub prior_chunk_count: Option<u64>,
    /// Forwarded to the daemon as `"force": true` in the reindex kickoff body.
    /// Set by `index --force` so the daemon clears its content-hash cache and
    /// re-embeds every file (otherwise unchanged files would be skipped on a
    /// warm daemon and `--force` would have no effect).
    pub force: bool,
    /// Hard wall-clock cap in seconds. Applied only when `timeout_explicit` is
    /// `true` (i.e. the user passed `--timeout N` explicitly). When `0` and
    /// `timeout_explicit` is `true`, the CLI waits forever (legacy behaviour).
    /// When `timeout_explicit` is `false`, this field is ignored and the CLI
    /// instead exits only on a genuine stall (see `stall_secs`).
    pub timeout_secs: u64,
    /// Whether `timeout_secs` was explicitly supplied by the user.
    ///
    /// When `false` (the default), the CLI uses progress-aware stall detection:
    /// it keeps waiting as long as the file-index counter advances within the
    /// `stall_secs` window. When `true`, `timeout_secs` is treated as a hard
    /// wall-clock cap regardless of progress (so `--timeout 120` reliably exits
    /// after exactly 120 s even if embedding is running).
    pub timeout_explicit: bool,
    /// How long (seconds) to wait without any progress before detaching.
    ///
    /// "Progress" means the per-file `indexed` counter has advanced since the
    /// last check. This window guards against a genuinely stalled pipeline
    /// (e.g. the embedder crashed silently) rather than a healthy but slow one.
    /// Default: 120 s. Only used when `timeout_explicit` is `false`.
    pub stall_secs: u64,
}

impl Default for ReindexOptions {
    fn default() -> Self {
        Self {
            verify_after: false,
            prior_chunk_count: None,
            force: false,
            timeout_secs: 600,
            timeout_explicit: false,
            stall_secs: 120,
        }
    }
}

/// Outcome of a reindex run, captured for the post-verify step and the final
/// summary line. `indexed` includes skipped files (the daemon emits one
/// `indexed++` per file regardless of whether it was hashed-skip or re-embedded).
///
/// Why: a single return type captures everything the caller needs to print a
/// summary line, run the post-verify check, and diagnose partial failures.
/// What: plain struct derived from SSE `complete` event fields.
/// Test: covered indirectly by `run_reindex_with` tests.
#[derive(Debug, Default, Clone, Copy)]
pub struct ReindexOutcome {
    pub indexed: u64,
    pub total_chunks: u64,
    pub skipped: u64,
    pub errors: u64,
    pub elapsed_ms: u64,
    pub completed: bool,
    /// Per-subsystem timings captured from the daemon's `complete` event
    /// `timings` payload. `None` when the daemon is an older version that
    /// didn't emit timings — caller renders a single-line summary in that case.
    pub timings: Option<ReindexTimings>,
}
