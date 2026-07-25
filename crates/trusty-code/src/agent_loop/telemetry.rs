//! Durable compression-effectiveness telemetry (epic #3866, Slice A #3867 +
//! Slice B #3868).
//!
//! Why: `cadence::CadenceOutcome` and the #2308 threshold compactor both
//! compute real token/ratio numbers every time they fire, and — before this
//! module — every one of those numbers was thrown away past a
//! `tracing::info!`/`tracing::error!` line (stderr-only, never aggregated)
//! or an in-memory-only cache that resets on session end. Owner directive:
//! "measure compression effectiveness, and use logging to track it." This
//! module is the durable, structured, cross-session, machine-aggregable
//! third channel `crate::logging`'s own "logs vs. events" doc note says
//! `tracing`/`crate::events` cannot serve — see `crates/trusty-code/src/
//! logging/mod.rs`.
//! What: [`CompressionEvent`] is one JSONL record per compression/
//! summarization event, appended (best-effort, mirroring
//! `trusty-agents::usage::append_usage`'s failure posture exactly) to
//! `~/.trusty-code/compression.jsonl` via [`write_compression_event`].
//! [`record_compaction_alarm`]/[`lifetime_compaction_alarm_count`] are
//! Slice B's durable "has threshold compaction EVER fired under cadence"
//! never-event alarm: a dedicated append-only log
//! (`~/.trusty-code/compaction_alarm.log`), one line per fire, counted by
//! line rather than read-modify-write so concurrent fires can never race
//! each other into a lost increment.
//! Test: `telemetry::tests::*`.

use std::fs::OpenOptions;
use std::io::{BufRead, Write as _};
use std::path::{Path, PathBuf};

/// [`CompressionEvent::surface`] value for the #2346 cadence compressor.
pub const SURFACE_TCODE_CADENCE: &str = "tcode-cadence";
/// [`CompressionEvent::surface`] value for the #2308 threshold compactor.
pub const SURFACE_TCODE_THRESHOLD: &str = "tcode-threshold";
/// [`CompressionEvent::surface`] value for a #3911 cadence floor-breach
/// backstop fire — see [`record_cadence_floor_breach`].
pub const SURFACE_TCODE_CADENCE_FLOOR_BREACH: &str = "tcode-cadence-floor-breach";

/// `surface_detail` used for every [`SURFACE_TCODE_CADENCE`] record.
pub const DETAIL_CADENCE: &str = "cadence";
/// `surface_detail` used for every [`SURFACE_TCODE_THRESHOLD`] record.
pub const DETAIL_THRESHOLD: &str = "threshold";
/// `surface_detail` used for every [`SURFACE_TCODE_CADENCE_FLOOR_BREACH`]
/// record.
pub const DETAIL_CADENCE_FLOOR_BREACH: &str = "cadence-floor-breach";

/// Filename of the durable compression-telemetry JSONL, under whatever data
/// directory the caller resolves (production: `crate::workstreams::
/// default_data_dir()`, i.e. `~/.trusty-code`).
const COMPRESSION_LOG_FILENAME: &str = "compression.jsonl";

/// Filename of Slice B's durable compaction-alarm log, one line per
/// `cadence: Some(_)` threshold-compaction fire.
const COMPACTION_ALARM_LOG_FILENAME: &str = "compaction_alarm.log";

/// Filename of the #3911 durable cadence floor-breach alarm log, one line
/// per turn `cadence::enforce_budget` exhausted every eligible entry and
/// STILL exceeded the overhead cap (`CadenceOutcome::within_budget ==
/// false`) — the never-event alarm for a cadence-level 60%-floor breach,
/// mirroring [`COMPACTION_ALARM_LOG_FILENAME`]'s identical append-only
/// design but for the cadence backstop rather than the threshold one.
const CADENCE_FLOOR_BREACH_ALARM_LOG_FILENAME: &str = "cadence_floor_breach_alarm.log";

/// Escape-hatch env var overriding [`default_data_dir`] for one process,
/// mirroring `cadence::CADENCE_TURNS_ENV_VAR`'s precedent. Test-only in
/// practice (production never sets it) — lets a test point every telemetry
/// write/read at an isolated temp directory instead of the real
/// `~/.trusty-code`.
pub const DATA_DIR_ENV_VAR: &str = "TCODE_TELEMETRY_DATA_DIR";

/// Serializes tests that mutate [`DATA_DIR_ENV_VAR`], mirroring
/// `cadence_env_helper::CADENCE_ENV_LOCK`'s identical rationale. `pub(crate)`
/// (not private) so other test modules mutating the SAME env var
/// (`agent_loop::compression_telemetry_tests`) share ONE lock —
/// `mode::MODE_ENV_LOCK`'s identical cross-module rationale. `#[cfg(test)]`
/// since production never touches this var at all.
#[cfg(test)]
pub(crate) static DATA_DIR_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Resolve the data directory every telemetry read/write in this module
/// uses: [`DATA_DIR_ENV_VAR`] when set (test isolation), otherwise
/// `crate::workstreams::default_data_dir()` (`~/.trusty-code`, production).
pub fn default_data_dir() -> PathBuf {
    std::env::var(DATA_DIR_ENV_VAR)
        .map(PathBuf::from)
        .unwrap_or_else(|_| crate::workstreams::default_data_dir())
}

/// One JSONL line: a single compression/summarization event (issue #3867).
///
/// Why: a flat, all-primitive shape keeps the log trivially grep-able and
/// loadable into jq/pandas/DuckDB for Slice C's report, and lets every field
/// be asserted individually in tests without a custom deserializer.
/// What: field names and semantics match issue #3867's schema exactly — see
/// the issue body for the authoritative field-by-field spec. `ratio` is
/// `tokens_after / tokens_before`, `0.0` when `tokens_before == 0` (never a
/// divide-by-zero or NaN, mirroring `tm compress`'s `log_compression_stats`
/// guard). `working_context_pct_after`/`overhead_pct_after` are `None` for
/// surfaces with no `CadenceOutcome` to derive them from (`agents-ws-summary`
/// per the issue; this crate never emits that surface).
/// Test: `tests::compression_event_serializes_expected_schema`,
/// `tests::ratio_guards_divide_by_zero`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct CompressionEvent {
    pub ts: String,
    pub session_id: Option<String>,
    pub surface: String,
    pub surface_detail: String,
    pub tokens_before: usize,
    pub tokens_after: usize,
    pub ratio: f64,
    pub working_context_pct_after: Option<u8>,
    pub overhead_pct_after: Option<u8>,
    pub compaction_event: bool,
    pub duration_ms: u64,
    pub rounds: usize,
}

impl CompressionEvent {
    /// Build a record with `ts` set to "now" (RFC3339 UTC, matching
    /// `trusty-agents::usage::UsageRecord.ts`'s convention) and `ratio`
    /// derived via [`compute_ratio`].
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        session_id: Option<String>,
        surface: &str,
        surface_detail: &str,
        tokens_before: usize,
        tokens_after: usize,
        working_context_pct_after: Option<u8>,
        overhead_pct_after: Option<u8>,
        compaction_event: bool,
        duration_ms: u64,
        rounds: usize,
    ) -> Self {
        Self {
            ts: chrono::Utc::now().to_rfc3339(),
            session_id,
            surface: surface.to_string(),
            surface_detail: surface_detail.to_string(),
            tokens_before,
            tokens_after,
            ratio: compute_ratio(tokens_before, tokens_after),
            working_context_pct_after,
            overhead_pct_after,
            compaction_event,
            duration_ms,
            rounds,
        }
    }
}

/// `tokens_after / tokens_before`, `0.0` when `tokens_before == 0`.
///
/// Why: never divide by zero, never produce NaN — a JSONL consumer (Slice
/// C's report, or a human `jq`-ing the file) must never have to guard
/// against either.
/// Test: `tests::ratio_guards_divide_by_zero`, `tests::ratio_is_after_over_before`.
pub fn compute_ratio(tokens_before: usize, tokens_after: usize) -> f64 {
    if tokens_before == 0 {
        return 0.0;
    }
    tokens_after as f64 / tokens_before as f64
}

/// Resolve `<data_dir>/compression.jsonl`.
pub fn compression_log_path(data_dir: &Path) -> PathBuf {
    data_dir.join(COMPRESSION_LOG_FILENAME)
}

/// Resolve `<data_dir>/compaction_alarm.log`.
pub fn compaction_alarm_log_path(data_dir: &Path) -> PathBuf {
    data_dir.join(COMPACTION_ALARM_LOG_FILENAME)
}

/// Resolve `<data_dir>/cadence_floor_breach_alarm.log` (issue #3911).
pub fn cadence_floor_breach_alarm_log_path(data_dir: &Path) -> PathBuf {
    data_dir.join(CADENCE_FLOOR_BREACH_ALARM_LOG_FILENAME)
}

/// Append `event` as one JSONL line to `<data_dir>/compression.jsonl`.
///
/// Why: emission must never fail the compaction/cadence operation it
/// instruments — this is observability, not control flow.
/// What: best-effort `mkdir -p` of `data_dir`, then opens the file in
/// `create + append` mode and writes one `serde_json::to_string(event)` line
/// followed by `\n`, flushing so the bytes are visible to a concurrent
/// reader. Any I/O or serialization failure logs at `debug!` and returns —
/// mirrors `trusty-agents::usage::append_usage`'s failure posture exactly
/// (issue #3867 acceptance criteria: "a broken/unwritable log directory must
/// not fail the compression/summarization operation itself").
/// Test: `tests::write_compression_event_creates_and_appends`,
/// `tests::write_compression_event_unwritable_dir_does_not_panic`.
pub fn write_compression_event(data_dir: &Path, event: &CompressionEvent) {
    let _ = std::fs::create_dir_all(data_dir);
    let line = match serde_json::to_string(event) {
        Ok(s) => format!("{s}\n"),
        Err(e) => {
            tracing::debug!(error = %e, "compression telemetry: serialize failed");
            return;
        }
    };
    let path = compression_log_path(data_dir);
    match OpenOptions::new().create(true).append(true).open(&path) {
        Ok(mut f) => {
            if let Err(e) = f.write_all(line.as_bytes()) {
                tracing::debug!(error = %e, path = %path.display(), "compression telemetry: write failed");
                return;
            }
            let _ = f.flush();
        }
        Err(e) => {
            tracing::debug!(error = %e, path = %path.display(), "compression telemetry: open failed");
        }
    }
}

/// Append one line to `<data_dir>/compaction_alarm.log`, recording that a
/// `cadence: Some(_)` threshold-compaction fire just happened (issue #3868).
///
/// Why: a durable, cross-session, cheap-to-query answer to "has threshold
/// compaction ever fired under cadence" needs a signal that survives past
/// the `tracing::error!` at the call site and the in-memory `Transcript`
/// counter, both of which reset on session end. Append-only (one line per
/// fire, never read-modify-write) so concurrent fires from different
/// sessions can never race each other into a lost increment — the count is
/// always "number of lines", never a stored integer that can be
/// clobbered by two writers reading the same stale value.
/// What: best-effort, matching [`write_compression_event`]'s failure
/// posture. `cadence: None` runs must NEVER call this — see
/// `agent_loop::AgentLoop::maybe_compact_transcript`'s call site, which only
/// reaches this function inside its `self.config.cadence.is_some()` branch.
/// Test: `tests::record_compaction_alarm_appends_one_line_per_call`,
/// `tests::record_compaction_alarm_unwritable_dir_does_not_panic`.
pub fn record_compaction_alarm(data_dir: &Path) {
    let _ = std::fs::create_dir_all(data_dir);
    let path = compaction_alarm_log_path(data_dir);
    let line = format!("{}\n", chrono::Utc::now().to_rfc3339());
    match OpenOptions::new().create(true).append(true).open(&path) {
        Ok(mut f) => {
            if let Err(e) = f.write_all(line.as_bytes()) {
                tracing::debug!(error = %e, path = %path.display(), "compaction alarm: write failed");
                return;
            }
            let _ = f.flush();
        }
        Err(e) => {
            tracing::debug!(error = %e, path = %path.display(), "compaction alarm: open failed");
        }
    }
}

/// Count how many times [`record_compaction_alarm`] has ever fired for
/// `data_dir` — the lifetime, cross-session, cross-restart alarm count
/// exposed on `session.get_context_budget` as `lifetime_compaction_alarm_count`
/// (issue #3868).
///
/// Why: reading a line count from disk on every query is cheap (this is
/// meant to stay a NEVER-event — the file should almost always be empty or
/// absent) and trivially correct — no separate stored counter that could
/// drift from the log it's supposedly summarizing.
/// What: `0` when the file is missing or unreadable (never an error — a
/// fresh install with no alarm history is the common case, not a fault);
/// otherwise the number of non-empty lines.
/// Test: `tests::lifetime_compaction_alarm_count_zero_when_missing`,
/// `tests::lifetime_compaction_alarm_count_matches_fire_count`.
pub fn lifetime_compaction_alarm_count(data_dir: &Path) -> u64 {
    let path = compaction_alarm_log_path(data_dir);
    let Ok(file) = std::fs::File::open(&path) else {
        return 0;
    };
    std::io::BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter(|l| !l.trim().is_empty())
        .count() as u64
}

/// Append one line to `<data_dir>/cadence_floor_breach_alarm.log`, recording
/// that `cadence::enforce_budget` exhausted every eligible entry and STILL
/// exceeded the overhead cap this turn (issue #3911's backstop).
///
/// Why: before this, a cadence-level 60%-floor breach produced only an
/// in-process `tracing::warn!` inside `cadence::enforce_budget` — invisible
/// outside stderr, never durable, never counted. The load-realistic soak
/// (epic #3866, PR #3909) proved this breach can and does happen under
/// heavy load (floor 48%) while EVERY other alarm signal
/// (`lifetime_compaction_alarm_count`, `TranscriptRecord.compaction_events`)
/// stayed at 0 — because those signals watch the THRESHOLD compactor, a
/// mechanically independent trigger cadence's own floor breach never
/// crosses (see [`record_cadence_floor_breach`]'s docs for why). This is
/// the durable signal that closes that gap. Same append-only,
/// count-by-lines design as [`record_compaction_alarm`] for the identical
/// reason: concurrent fires from different sessions can never race each
/// other into a lost increment.
/// What: best-effort, matching [`write_compression_event`]'s failure
/// posture.
/// Test: `tests::record_cadence_floor_breach_alarm_appends_one_line_per_call`,
/// `tests::record_cadence_floor_breach_alarm_unwritable_dir_does_not_panic`.
pub fn record_cadence_floor_breach_alarm(data_dir: &Path) {
    let _ = std::fs::create_dir_all(data_dir);
    let path = cadence_floor_breach_alarm_log_path(data_dir);
    let line = format!("{}\n", chrono::Utc::now().to_rfc3339());
    match OpenOptions::new().create(true).append(true).open(&path) {
        Ok(mut f) => {
            if let Err(e) = f.write_all(line.as_bytes()) {
                tracing::debug!(error = %e, path = %path.display(), "cadence floor-breach alarm: write failed");
                return;
            }
            let _ = f.flush();
        }
        Err(e) => {
            tracing::debug!(error = %e, path = %path.display(), "cadence floor-breach alarm: open failed");
        }
    }
}

/// Count how many times [`record_cadence_floor_breach_alarm`] has ever
/// fired for `data_dir` — the lifetime, cross-session, cross-restart alarm
/// count exposed on `session.get_context_budget` as
/// `lifetime_cadence_floor_breach_count` (issue #3911).
///
/// What: `0` when the file is missing or unreadable (the common,
/// never-event case); otherwise the number of non-empty lines. Mirrors
/// [`lifetime_compaction_alarm_count`]'s identical read-fresh-from-disk
/// contract.
/// Test: `tests::lifetime_cadence_floor_breach_count_zero_when_missing`,
/// `tests::lifetime_cadence_floor_breach_count_matches_fire_count`.
pub fn lifetime_cadence_floor_breach_count(data_dir: &Path) -> u64 {
    let path = cadence_floor_breach_alarm_log_path(data_dir);
    let Ok(file) = std::fs::File::open(&path) else {
        return 0;
    };
    std::io::BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter(|l| !l.trim().is_empty())
        .count() as u64
}

/// Session-scoped working-context low-water mark, read fresh from the
/// durable `compression.jsonl` log (issue #3912).
///
/// Why: `session.get_context_budget` cached only the MOST RECENT
/// `ContextBudgetSnapshot` — a single point-in-time value that a coarse,
/// once-per-`task.run`-call poll can easily miss the real dip on (the
/// load-realistic soak, epic #3866, observed the RPC report 98-99% while
/// the durable JSONL recorded a 48-60% floor for the SAME run). Every
/// cadence/threshold fire already writes its `working_context_pct_after`
/// to this log (issue #3867) — this is the query that turns "every sample
/// this session ever produced" into the one number an operator actually
/// needs: how low did it REALLY get.
/// What: scans `<data_dir>/compression.jsonl` line by line (best-effort,
/// matching every other reader in this module — a missing file, an
/// unreadable line, or a record with no `working_context_pct_after` is
/// silently skipped, never an error), keeping only rows whose `session_id`
/// matches `session_id` exactly. Returns `(None, 0)` when the file is
/// missing or no matching sample carries a percentage; otherwise
/// `(Some(min), count)` — the lowest `working_context_pct_after` observed
/// for this session and how many samples that minimum was computed over.
/// Test: `tests::session_working_context_floor_missing_file_is_none`,
/// `tests::session_working_context_floor_tracks_minimum_for_session`,
/// `tests::session_working_context_floor_ignores_other_sessions`.
pub fn session_working_context_floor(data_dir: &Path, session_id: &str) -> (Option<u8>, usize) {
    let Ok(file) = std::fs::File::open(compression_log_path(data_dir)) else {
        return (None, 0);
    };
    let mut min_pct: Option<u8> = None;
    let mut count = 0usize;
    for line in std::io::BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(event) = serde_json::from_str::<CompressionEvent>(&line) else {
            continue;
        };
        if event.session_id.as_deref() != Some(session_id) {
            continue;
        }
        let Some(pct) = event.working_context_pct_after else {
            continue;
        };
        count += 1;
        min_pct = Some(min_pct.map_or(pct, |m| m.min(pct)));
    }
    (min_pct, count)
}

/// Derive `(working_context_pct, overhead_pct)` from a raw token measurement
/// against a model's context window, mirroring
/// `cadence::CadenceOutcome::working_context_pct`/`overhead_pct`'s own
/// saturating arithmetic (never a divide-by-zero, never > 100).
///
/// Why: shared by both `AgentLoop::maybe_compact_transcript` (which has no
/// `CadenceOutcome` to read these from) and callers that already have one,
/// so the two call sites' percentages can never drift apart from cadence's
/// own formula.
/// What: `(None, None)` for a zero `context_window` (percentages are
/// meaningless without a real window); otherwise `overhead_pct =
/// (tokens * 100 / context_window).min(100)`, `working_context_pct = 100 -
/// overhead_pct`.
/// Test: `tests::context_pcts_zero_window_is_none`,
/// `tests::context_pcts_matches_saturating_formula`.
pub fn context_pcts(tokens: usize, context_window: usize) -> (Option<u8>, Option<u8>) {
    if context_window == 0 {
        return (None, None);
    }
    let overhead_pct = (tokens.saturating_mul(100) / context_window).min(100) as u8;
    (Some(100 - overhead_pct), Some(overhead_pct))
}

/// Build and emit the [`SURFACE_TCODE_THRESHOLD`] telemetry record for one
/// `AgentLoop::maybe_compact_transcript` fire, and — when `cadence_enabled`
/// — the Slice B durable alarm line alongside it (issues #3867, #3868).
///
/// Why: keeps `AgentLoop::maybe_compact_transcript` a thin call rather than
/// inlining the event construction + percentage derivation at that call
/// site (this crate's 500-SLOC production file cap).
/// What: `compaction_event` is always `true` (issue #3867: `true` only for
/// `tcode-threshold` fires — this function is only ever called on one).
/// `cadence_enabled` gates the alarm line only — the JSONL record itself is
/// unconditional, per issue #3867's spec.
/// Test: `tests::record_threshold_event_writes_jsonl_and_alarm_when_cadence_enabled`,
/// `tests::record_threshold_event_no_alarm_when_cadence_disabled`.
#[allow(clippy::too_many_arguments)]
pub fn record_threshold_event(
    data_dir: &Path,
    session_id: Option<String>,
    tokens_before: usize,
    tokens_after: usize,
    context_window: usize,
    duration_ms: u64,
    cadence_enabled: bool,
) {
    let (working_context_pct_after, overhead_pct_after) =
        context_pcts(tokens_after, context_window);
    write_compression_event(
        data_dir,
        &CompressionEvent::new(
            session_id,
            SURFACE_TCODE_THRESHOLD,
            DETAIL_THRESHOLD,
            tokens_before,
            tokens_after,
            working_context_pct_after,
            overhead_pct_after,
            true,
            duration_ms,
            1,
        ),
    );
    if cadence_enabled {
        record_compaction_alarm(data_dir);
    }
}

/// Build and emit the [`SURFACE_TCODE_CADENCE`] telemetry record for one
/// `AgentLoop::maybe_cadence_compress` call whose `outcome.rounds > 0`
/// (issue #3867).
///
/// Why: same file-size rationale as [`record_threshold_event`].
/// What: `compaction_event` is always `false` (cadence fires are never the
/// Slice B alarm signal). Caller must only invoke this when
/// `outcome.rounds > 0` — see the issue's "only emit when something
/// actually happened" note.
/// Test: `tests::record_cadence_event_writes_jsonl`.
#[allow(clippy::too_many_arguments)]
pub fn record_cadence_event(
    data_dir: &Path,
    session_id: Option<String>,
    tokens_before: usize,
    tokens_after: usize,
    context_window: usize,
    duration_ms: u64,
    rounds: usize,
) {
    let (working_context_pct_after, overhead_pct_after) =
        context_pcts(tokens_after, context_window);
    write_compression_event(
        data_dir,
        &CompressionEvent::new(
            session_id,
            SURFACE_TCODE_CADENCE,
            DETAIL_CADENCE,
            tokens_before,
            tokens_after,
            working_context_pct_after,
            overhead_pct_after,
            false,
            duration_ms,
            rounds,
        ),
    );
}

/// Build and emit the [`SURFACE_TCODE_CADENCE_FLOOR_BREACH`] telemetry
/// record plus the durable [`record_cadence_floor_breach_alarm`] line, for
/// one `AgentLoop::maybe_cadence_compress` call whose
/// `CadenceOutcome::within_budget` was `false` (issue #3911).
///
/// Why: `cadence::enforce_budget`'s own "documented floor" case (active
/// zone shrunk to zero, still over the overhead cap) previously only
/// logged `tracing::warn!` from deep inside that function — no durable
/// record, no counter, and critically NOT the same alarm the #2308
/// threshold compactor's OWN breach uses
/// (`record_threshold_event`/`lifetime_compaction_alarm_count`), because
/// that alarm is keyed to a MECHANICALLY INDEPENDENT trigger: threshold
/// fires only once the raw transcript crosses 75% of the context window
/// (`CompactionConfig::for_context_window`), a laxer bound than cadence's
/// own 40%-overhead enforcement cap — so by the time cadence has already
/// compacted as hard as it structurally can (`enforce_budget` shrinks the
/// active zone to zero, re-compacting at every step) and still exceeds
/// its OWN cap, the transcript is nowhere near threshold's separate,
/// higher bound. The load-realistic soak (epic #3866, PR #3909) proved
/// this empirically: a 48% floor breach produced zero threshold events,
/// zero alarm-count increments, and zero visible signal anywhere but a
/// stderr line. This function is the durable, countable trigger that
/// closes that gap — always called (unconditionally, unlike
/// `record_threshold_event`'s cadence-gated alarm) because this call site
/// only exists on the cadence-enabled path in the first place
/// (`AgentLoop::maybe_cadence_compress` already returns early when
/// `self.config.cadence` is `None`).
/// What: writes a `compaction_event: true` JSONL row (this genuinely IS an
/// alarm-worthy event — the same semantic `record_threshold_event` uses
/// `true` for) tagged `surface: "tcode-cadence-floor-breach"`, then
/// unconditionally appends the durable alarm line.
/// Test: `tests::record_cadence_floor_breach_writes_jsonl_and_alarm`.
#[allow(clippy::too_many_arguments)]
pub fn record_cadence_floor_breach(
    data_dir: &Path,
    session_id: Option<String>,
    tokens_before: usize,
    tokens_after: usize,
    context_window: usize,
    duration_ms: u64,
    rounds: usize,
) {
    let (working_context_pct_after, overhead_pct_after) =
        context_pcts(tokens_after, context_window);
    write_compression_event(
        data_dir,
        &CompressionEvent::new(
            session_id,
            SURFACE_TCODE_CADENCE_FLOOR_BREACH,
            DETAIL_CADENCE_FLOOR_BREACH,
            tokens_before,
            tokens_after,
            working_context_pct_after,
            overhead_pct_after,
            true,
            duration_ms,
            rounds,
        ),
    );
    record_cadence_floor_breach_alarm(data_dir);
}

/// Test-only helper: run `f` with [`DATA_DIR_ENV_VAR`] pointed at `dir`,
/// serialized on [`DATA_DIR_ENV_LOCK`] so parallel `cargo test` threads never
/// race each other's env mutation — mirrors
/// `cadence_env_helper::with_cadence_env`'s identical rationale.
///
/// Why: production telemetry always writes/reads real `~/.trusty-code`; any
/// test exercising `AgentLoop::maybe_compact_transcript`/
/// `maybe_cadence_compress` or `SessionRegistry::record_context_budget` end
/// to end needs an isolated directory instead, without a second production
/// code path.
#[cfg(test)]
pub(crate) async fn with_data_dir_env<T>(dir: &Path, f: impl FnOnce() -> T) -> T {
    let _guard = DATA_DIR_ENV_LOCK.lock().await;
    // SAFETY: test-only env mutation, serialized by `DATA_DIR_ENV_LOCK`.
    unsafe {
        std::env::set_var(DATA_DIR_ENV_VAR, dir);
    }
    let result = f();
    unsafe {
        std::env::remove_var(DATA_DIR_ENV_VAR);
    }
    result
}

/// Async twin of [`with_data_dir_env`] for loop-level tests that must await
/// an `AgentLoop::run`/`run_with_transcript` call WHILE the env var is set
/// (a plain `FnOnce() -> T` closure cannot itself `.await`).
///
/// Why: without this, every loop-integration test needed to hand-inline the
/// lock/set/run/clear sequence, duplicating the `unsafe` env mutation at
/// each call site. Centralising it here keeps that `unsafe` block in exactly
/// one place, matching [`with_data_dir_env`]'s same rationale.
#[cfg(test)]
pub(crate) async fn with_data_dir_env_fut<T>(
    dir: &Path,
    fut: impl std::future::Future<Output = T>,
) -> T {
    let _guard = DATA_DIR_ENV_LOCK.lock().await;
    // SAFETY: test-only env mutation, serialized by `DATA_DIR_ENV_LOCK`.
    unsafe {
        std::env::set_var(DATA_DIR_ENV_VAR, dir);
    }
    let result = fut.await;
    unsafe {
        std::env::remove_var(DATA_DIR_ENV_VAR);
    }
    result
}

/// Test-only helper: a fresh, uniquely-named, already-created temp directory
/// under `std::env::temp_dir()`, for tests that need an isolated telemetry
/// write target.
///
/// Why: every loop-integration test that isolates its telemetry writes
/// previously duplicated this exact `process_id + nanosecond-timestamp`
/// naming scheme at its own call site (`compression_telemetry_tests.rs`,
/// `telemetry_tests.rs`, `tests/cadence.rs`'s `forced_degradation_*` test).
/// `pub(crate)` so every `agent_loop` test submodule can share ONE
/// implementation instead of re-deriving the same uniqueness scheme —
/// consolidating them is also what makes issue #3902's fix
/// (`AgentLoopConfig::telemetry_data_dir` injection replacing the raced
/// `TCODE_TELEMETRY_DATA_DIR` env var) a one-line call at each of the
/// newly-isolated call sites instead of a repeated inline block.
/// What: `label` only needs to distinguish this call site from sibling
/// calls in the same test binary — the pid + nanosecond timestamp already
/// guarantees cross-process and cross-call uniqueness on its own.
#[cfg(test)]
pub(crate) fn test_temp_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "tcode-telemetry-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[cfg(test)]
#[path = "telemetry_tests.rs"]
mod tests;
