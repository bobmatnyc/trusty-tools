//! Daily per-palace drawer-count history (#9283).
//!
//! Why: #8729 lost drawers silently — nothing recorded how many drawers a
//! palace held, so a drop could not be seen, let alone matched against the
//! deletion journal. The trusty-memory 1.0 gate wants seven days with no
//! unexplained drop, which needs a count per palace per day.
//! What: the daemon appends one line per palace per UTC day to
//! `<data_root>/drawer_counts.jsonl` ([`take_snapshot`], driven by
//! [`spawn_snapshot_task`]). Counts come from the open handle when the palace is
//! resident and from its `kg.redb` B-tree header otherwise, so no palace is
//! opened to be counted (#1924). The snapshot holds the idle-evict
//! [`EvictGate`] so no sweep takes a resident handle away mid-count, and
//! re-tries an unreadable palace a few times before giving up. A palace that
//! still cannot be read is recorded as `unavailable` with a null count and the
//! reason, never as zero, and logged at `warn`. Lines older than
//! [`RETENTION_DAYS`] are pruned on the next write. [`analysis`] compares
//! adjacent counts against `maintenance_deletions.jsonl`; the doctor check and
//! `doctor --drawer-report` read its verdicts.
//! The file is a primary audit journal under ADR-0067 D2: the line schema is
//! frozen at `v: 1`, new fields are additive, and a line with a newer `v` is
//! kept verbatim by the writer and refused by the reader.
//! Test: `drawer_counts::tests` (see each function's `Test:` line).

pub mod analysis;
#[cfg(test)]
mod tests;

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use trusty_common::memory_core::{Palace, PalaceHandle, PalaceRegistry};

use crate::console_metrics::disk_stats;
use crate::idle_evict::EvictGate;
use crate::transport::methods::HEALTH_PROBE_PALACE;

/// File name of the history, directly under the palace data root.
pub const DRAWER_COUNTS_FILENAME: &str = "drawer_counts.jsonl";
/// The line schema this binary writes and reads.
pub const SCHEMA_VERSION: u32 = 1;
/// How many days of lines the writer keeps.
pub const RETENTION_DAYS: i64 = 90;
/// How often the daemon task checks whether today's snapshot is due.
///
/// Why hourly rather than daily: `tokio` sleeps on a monotonic clock that
/// stops while a laptop sleeps, so a 24 h timer drifts past whole UTC days.
/// The once-per-day guard in [`take_snapshot`] keeps it to one line a day.
pub const SNAPSHOT_CHECK_INTERVAL: Duration = Duration::from_secs(60 * 60);
/// Delay before the first snapshot.
///
/// Why (#9283): under the 60 s first idle-evict tick, so a restart's
/// hydrated palaces are counted from their resident handles. The
/// [`EvictGate`], not this number, is what keeps the two apart.
pub const SNAPSHOT_FIRST_DELAY: Duration = Duration::from_secs(30);
/// Waits before each re-try of a palace whose count could not be read.
///
/// Why (#9283): a handle leaving the registry can keep its `kg.redb` locked a
/// moment longer (a background task still holds its store); three short
/// re-tries cover that without stalling a snapshot for long.
pub const RETRY_DELAYS: [Duration; 3] = [
    Duration::from_millis(250),
    Duration::from_secs(1),
    Duration::from_secs(2),
];
/// How long a writer waits for the history's file lock.
const LOCK_TIMEOUT: Duration = Duration::from_secs(10);

/// Where a recorded count came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CountSource {
    /// The `DRAWERS` row count, read through the resident handle's store.
    Cache,
    /// The `DRAWERS` table length in a closed palace's `kg.redb`.
    Disk,
    /// The palace could not be read; the line's `reason` says why.
    Unavailable,
    /// Not a count: an operator acknowledgement of a drop (`doctor --ack-drop`).
    Ack,
}

/// An operator acknowledgement that `count` drawers left `palace` on `day`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ack {
    /// Drawers the operator accounts for.
    pub count: usize,
    /// Why they left.
    pub reason: String,
    /// The snapshot day whose drop this explains.
    pub day: NaiveDate,
}

/// One history line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CountLine {
    /// Line schema version; always [`SCHEMA_VERSION`] when this binary writes.
    pub v: u32,
    /// When the count was taken (or the ack written).
    pub at: DateTime<Utc>,
    /// Resolved palace id, never an alias (#5036).
    pub palace: String,
    /// Drawer count; `None` when unavailable and on an ack line.
    pub drawers: Option<usize>,
    /// Where the count came from.
    pub src: CountSource,
    /// Present only on an ack line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ack: Option<Ack>,
    /// Why the count is unavailable; present only on an unavailable line
    /// (#9283). Operator-facing, carries no drawer content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl CountLine {
    /// The UTC day this line belongs to.
    pub fn day(&self) -> NaiveDate {
        self.at.date_naive()
    }
}

/// The history read back.
#[derive(Debug, Default)]
pub struct History {
    /// Every line this binary can read, in file order.
    pub lines: Vec<CountLine>,
    /// Lines a newer binary wrote. Their presence makes readers refuse.
    pub newer: usize,
    /// Lines that did not parse (a torn write).
    pub malformed: usize,
}

/// How one raw line classifies.
enum Parsed {
    Current(CountLine),
    Newer,
    Malformed,
}

fn classify(raw: &str) -> Parsed {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Parsed::Malformed;
    };
    let v = value.get("v").and_then(serde_json::Value::as_u64);
    if v.is_some_and(|v| v > u64::from(SCHEMA_VERSION)) {
        return Parsed::Newer;
    }
    match serde_json::from_value::<CountLine>(value) {
        Ok(line) => Parsed::Current(line),
        Err(_) => Parsed::Malformed,
    }
}

/// Path of the history for a palace data root.
pub fn history_path(data_root: &Path) -> PathBuf {
    data_root.join(DRAWER_COUNTS_FILENAME)
}

/// The history file's text; a missing file is empty.
fn read_text(path: &Path) -> Result<String> {
    match std::fs::read_to_string(path) {
        Ok(t) => Ok(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
    }
}

/// Parse history text into lines this binary understands.
pub fn parse_history(text: &str) -> History {
    let mut out = History::default();
    for raw in text.lines().filter(|l| !l.trim().is_empty()) {
        match classify(raw) {
            Parsed::Current(line) => out.lines.push(line),
            Parsed::Newer => out.newer += 1,
            Parsed::Malformed => out.malformed += 1,
        }
    }
    out
}

/// Read the history under `data_root`. Never writes.
///
/// Test: `future_schema_line_is_refused_not_dropped`.
pub fn read_history(data_root: &Path) -> Result<History> {
    Ok(parse_history(&read_text(&history_path(data_root))?))
}

/// What one [`take_snapshot`] call changed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SnapshotOutcome {
    /// Lines appended for today.
    pub written: usize,
    /// Lines dropped for age.
    pub pruned: usize,
}

/// One palace's count, or the reason it has none.
struct Counted {
    drawers: Option<usize>,
    src: CountSource,
    reason: Option<String>,
}

impl Counted {
    /// Count a resident palace through its own open store.
    ///
    /// Why (#9283): the handle's in-memory list also holds L1-snapshot
    /// drawers the store no longer has (a sandbox copy of the live root read
    /// 564 there against 559 rows on disk), so a palace counted resident one
    /// day and from disk the next showed a drop no journal explains.
    /// What: the `DRAWERS` row count (decoded plus skipped rows) read through
    /// the daemon's own handle — the figure [`disk_stats::read`] returns —
    /// so no second open is made. A failed read is unavailable, never zero.
    fn cached(handle: &PalaceHandle) -> Self {
        match handle.kg.load_drawers_with_skipped() {
            Ok((rows, skipped)) => Self {
                drawers: Some(rows.len() + skipped),
                src: CountSource::Cache,
                reason: None,
            },
            Err(e) => Self {
                drawers: None,
                src: CountSource::Unavailable,
                reason: Some(format!("resident store read failed: {e:#}")),
            },
        }
    }
}

/// Count one palace without opening it.
///
/// Why: see the module doc and `console_metrics::count_palace`, whose split
/// this mirrors for the drawer count alone.
/// What: `peek` first; on a miss, [`disk_stats::read`]. A disk read that fails
/// gets one more `peek`, because the daemon may have opened the palace between
/// the two calls and its own lock is what refused the read.
/// Test: `snapshot_records_unavailable_not_zero_for_a_write_held_palace`.
fn count_drawers(registry: &PalaceRegistry, info: &Palace) -> Counted {
    if let Some(handle) = registry.peek(&info.id) {
        return Counted::cached(&handle);
    }
    match disk_stats::read(&info.data_dir) {
        Ok(stats) => Counted {
            drawers: Some(stats.drawer_count),
            src: CountSource::Disk,
            reason: None,
        },
        Err(reason) => match registry.peek(&info.id) {
            Some(handle) => Counted::cached(&handle),
            None => Counted {
                drawers: None,
                src: CountSource::Unavailable,
                reason: Some(reason),
            },
        },
    }
}

/// Count `todo`, re-trying each unreadable palace after each of `delays`.
///
/// Why (#9283): a handle on its way out of the registry can hold `kg.redb`
/// after `peek` stops finding it; one read per palace recorded such palaces
/// unavailable although the daemon itself held them.
/// What: one pass, then per delay: sleep, and recount (re-peek included) only
/// the palaces still without a count. Results follow `todo`'s order. Blocks,
/// so it runs on the blocking pool.
/// Test: `snapshot_counts_a_palace_whose_handle_is_still_closing`.
fn count_with_retries(
    registry: &PalaceRegistry,
    todo: &[&Palace],
    delays: &[Duration],
) -> Vec<Counted> {
    let mut out: Vec<Counted> = todo.iter().map(|p| count_drawers(registry, p)).collect();
    for delay in delays {
        let pending: Vec<usize> = (0..out.len())
            .filter(|&i| out[i].drawers.is_none())
            .collect();
        if pending.is_empty() {
            break;
        }
        std::thread::sleep(*delay);
        for i in pending {
            out[i] = count_drawers(registry, todo[i]);
        }
    }
    out
}

/// Record today's drawer count for every palace not yet recorded today.
///
/// Why: the doctor check needs a daily count per palace (#9283), and daemon
/// restarts must not stack several lines for one day.
/// What: lists the palaces under `data_root` (skipping the health-probe
/// palace), then, under the history's file lock, reads the file, drops lines
/// older than [`RETENTION_DAYS`], skips every palace that already has a line
/// for `now`'s UTC day, counts the rest with [`count_drawers`], and rewrites
/// the file through a temp file and a rename. A line this binary cannot read
/// is kept verbatim. Nothing is written when nothing changed.
///
/// # Errors
///
/// Listing the palaces, taking the lock, or reading or writing the file.
///
/// Test: `snapshot_task_writes_one_line_per_palace_per_day`,
/// `history_prunes_to_ninety_days`, `future_schema_line_is_refused_not_dropped`.
pub fn take_snapshot(
    registry: &PalaceRegistry,
    data_root: &Path,
    now: DateTime<Utc>,
) -> Result<SnapshotOutcome> {
    let palaces: Vec<Palace> = PalaceRegistry::list_palaces(data_root)?
        .into_iter()
        .filter(|p| p.id.as_str() != HEALTH_PROBE_PALACE)
        .collect();
    let path = history_path(data_root);
    trusty_common::file_lock::with_exclusive_lock_timeout(&path, LOCK_TIMEOUT, || {
        snapshot_locked(registry, &palaces, &path, now)
    })
    .map_err(|e| anyhow!("lock {}: {e}", path.display()))?
}

fn snapshot_locked(
    registry: &PalaceRegistry,
    palaces: &[Palace],
    path: &Path,
    now: DateTime<Utc>,
) -> Result<SnapshotOutcome> {
    let text = read_text(path)?;
    let today = now.date_naive();
    let cutoff = now - chrono::Duration::days(RETENTION_DAYS);
    let mut kept: Vec<String> = Vec::new();
    let mut done: HashSet<String> = HashSet::new();
    let mut outcome = SnapshotOutcome::default();
    for raw in text.lines().filter(|l| !l.trim().is_empty()) {
        if let Parsed::Current(line) = classify(raw) {
            if line.at < cutoff {
                outcome.pruned += 1;
                continue;
            }
            if line.ack.is_none() && line.day() == today {
                done.insert(line.palace);
            }
        }
        // A newer or torn line is kept: this binary never drops what it
        // cannot read (ADR-0067 D2).
        kept.push(raw.to_string());
    }
    let todo: Vec<&Palace> = palaces
        .iter()
        .filter(|p| !done.contains(p.id.as_str()))
        .collect();
    let counted = count_with_retries(registry, &todo, &RETRY_DELAYS);
    for (info, c) in todo.iter().zip(counted) {
        if let Some(reason) = &c.reason {
            // #9283: an uncountable palace can never go red, so say so loudly.
            tracing::warn!(
                palace = %info.id,
                "#9283: drawer count unavailable, recorded as unavailable: {reason}"
            );
        }
        let line = CountLine {
            v: SCHEMA_VERSION,
            at: now,
            palace: info.id.as_str().to_string(),
            drawers: c.drawers,
            src: c.src,
            ack: None,
            reason: c.reason,
        };
        kept.push(serde_json::to_string(&line).context("serialize count line")?);
        outcome.written += 1;
    }
    if outcome.written > 0 || outcome.pruned > 0 {
        replace_file(path, &kept)?;
    }
    Ok(outcome)
}

/// Write `lines` to a temp file beside `path`, then rename it over `path`.
fn replace_file(path: &Path, lines: &[String]) -> Result<()> {
    let tmp = path.with_extension("jsonl.tmp");
    let mut file =
        std::fs::File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
    for line in lines {
        writeln!(file, "{line}").with_context(|| format!("write {}", tmp.display()))?;
    }
    file.sync_all()
        .with_context(|| format!("sync {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))
}

/// Append an operator acknowledgement for a drop in `palace`.
///
/// Why: a drop with no journal record (a removal made before user forgets
/// were journaled, or a `*.v2-incompatible` reset the operator accepts) keeps
/// the check red until the operator accounts for it (#9283 design §2).
/// What: under the file lock, finds `palace`'s newest snapshot day (or uses
/// `day`), and appends one `src: "ack"` line. Returns the line written.
///
/// # Errors
///
/// A zero count, an empty reason, a palace with no snapshot, or I/O.
///
/// Test: `an_ack_explains_an_unjournaled_drop`.
pub fn append_ack(
    data_root: &Path,
    palace: &str,
    count: usize,
    reason: &str,
    day: Option<NaiveDate>,
    now: DateTime<Utc>,
) -> Result<CountLine> {
    if count == 0 || reason.trim().is_empty() {
        bail!("--ack-drop needs --count above 0 and a non-empty --reason");
    }
    let path = history_path(data_root);
    trusty_common::file_lock::with_exclusive_lock_timeout(&path, LOCK_TIMEOUT, || {
        let history = parse_history(&read_text(&path)?);
        let newest = history
            .lines
            .iter()
            .filter(|l| l.palace == palace && l.ack.is_none())
            .map(CountLine::day)
            .max();
        let Some(day) = day.or(newest) else {
            bail!("palace '{palace}' has no drawer-count snapshot to acknowledge");
        };
        let line = CountLine {
            v: SCHEMA_VERSION,
            at: now,
            palace: palace.to_string(),
            drawers: None,
            src: CountSource::Ack,
            ack: Some(Ack {
                count,
                reason: reason.trim().to_string(),
                day,
            }),
            reason: None,
        };
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        let text = format!("{}\n", serde_json::to_string(&line)?);
        file.write_all(text.as_bytes())
            .with_context(|| format!("append {}", path.display()))?;
        Ok(line)
    })
    .map_err(|e| anyhow!("lock {}: {e}", path.display()))?
}

/// Spawn the daemon's daily snapshot task (#9283).
///
/// Why: only the daemon can count a palace it holds open; the CLI doctor reads
/// the file this task writes.
/// What: a no-op returning `None` when the registry has no data root. Otherwise
/// one task: waits [`SNAPSHOT_FIRST_DELAY`], runs [`take_snapshot`] on the
/// blocking pool with `gate` held (#9283), then repeats every
/// [`SNAPSHOT_CHECK_INTERVAL`]. A failed snapshot is logged at `warn` and
/// retried on the next tick. Exits on the shutdown watch.
/// Test: `snapshot_loop_writes_at_start_and_stops_on_shutdown`.
pub fn spawn_snapshot_task(
    registry: Arc<PalaceRegistry>,
    gate: EvictGate,
    shutdown: watch::Receiver<bool>,
) -> Option<tokio::task::JoinHandle<()>> {
    let root = registry.data_root()?.to_path_buf();
    Some(spawn_snapshot_loop(
        registry,
        gate,
        root,
        SNAPSHOT_FIRST_DELAY,
        SNAPSHOT_CHECK_INTERVAL,
        shutdown,
    ))
}

fn spawn_snapshot_loop(
    registry: Arc<PalaceRegistry>,
    gate: EvictGate,
    data_root: PathBuf,
    first_delay: Duration,
    interval: Duration,
    mut shutdown: watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut wait = first_delay;
        loop {
            tokio::select! {
                _ = tokio::time::sleep(wait) => {}
                res = shutdown.changed() => if res.is_err() || *shutdown.borrow() { return; },
            }
            if *shutdown.borrow() {
                return;
            }
            let (reg, root, gate) = (registry.clone(), data_root.clone(), gate.clone());
            // #9283: hold the evict gate so no sweep pulls a resident handle
            // out of the registry while it is being counted.
            let run = move || {
                let _held = gate.lock();
                take_snapshot(&reg, &root, Utc::now())
            };
            match tokio::task::spawn_blocking(run).await {
                Ok(Ok(o)) if o.written > 0 || o.pruned > 0 => tracing::info!(
                    written = o.written,
                    pruned = o.pruned,
                    "#9283: drawer-count snapshot recorded"
                ),
                Ok(Ok(_)) => {}
                Ok(Err(e)) => tracing::warn!("#9283: drawer-count snapshot failed: {e:#}"),
                Err(e) => tracing::warn!("#9283: drawer-count snapshot task panicked: {e}"),
            }
            wait = interval;
        }
    })
}
