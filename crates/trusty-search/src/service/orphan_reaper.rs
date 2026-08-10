//! Automatic self-healing of orphaned index registrations (orphan self-heal).
//!
//! Why: MPM spins up an ephemeral git worktree per session under
//! `.worktrees/<uuid>/` and deletes it when the session ends. Each such root
//! can get registered as an index (auto-discovery or the colocated rescan);
//! when the worktree is removed the registration is left behind pointing at a
//! now-deleted path. Over a long-lived daemon these accumulate without bound —
//! production saw 485 dead registrations build up over 26 days, each re-read
//! and re-warned on every warm-boot and, worse, each holding an FSEvents watch
//! that pinned macOS `fseventsd` at ~100% CPU / 8 GB RSS. Nothing in the daemon
//! ever removed them; `prune-orphans` / `cleanup` were manual CLI commands
//! only, so an operator who never ran them accumulated the leak silently.
//!
//! What: two entry points share one safety predicate ([`is_reapable_orphan`]):
//!   * [`heal_boot_orphans`] — called once at warm-boot start; drops legacy
//!     (non-colocated) registrations whose `root_path` was deleted so they stop
//!     being re-read on every boot.
//!   * `server::tickers::spawn_orphan_reaper_ticker` — a periodic task that
//!     unregisters live indexes whose root vanished while the daemon runs
//!     (the actual accumulation vector, since the daemon ran continuously).
//!
//! Safety: a registration is only reaped when its `root_path` is missing AND
//! its immediate parent still exists. A deleted worktree leaves `.worktrees/`
//! behind (→ reap), whereas an unmounted external volume takes the whole parent
//! chain with it (→ do NOT reap; the data returns when the volume remounts).
//! Neither path ever deletes on-disk index *data* — only the registration is
//! removed — so a false-positive detection is always recoverable by
//! re-registering the path.
//!
//! Test: `is_reapable_orphan_*` and `heal_boot_orphans_*` unit tests below.

use std::path::Path;

use crate::service::persistence::PersistedIndex;

/// Environment variable overriding the reaper cadence, in seconds. `0` disables
/// the runtime reaper entirely; any unparseable value falls back to the default.
pub const REAP_INTERVAL_ENV: &str = "TRUSTY_ORPHAN_REAP_SECS";

/// Default runtime-reaper cadence: hourly.
///
/// Why: worktree churn happens on the timescale of minutes-to-hours, and a dead
/// registration costs an idle FSEvents watch, not correctness, so an hourly
/// sweep reclaims it promptly without adding meaningful wakeups. The boot-time
/// [`heal_boot_orphans`] pass covers everything accumulated while the daemon
/// was down, so the ticker only needs to catch roots deleted mid-run.
const DEFAULT_REAP_INTERVAL_SECS: u64 = 3600;

/// Resolve the runtime reaper cadence from the environment.
///
/// Why: lets an operator retune or disable (`TRUSTY_ORPHAN_REAP_SECS=0`) the
/// sweep without a rebuild — mirroring the idle-eviction ticker's env knob.
/// What: returns `None` when the var is exactly `0` (reaper disabled),
/// `Some(n)` for a positive integer, and `Some(DEFAULT_REAP_INTERVAL_SECS)`
/// when unset or unparseable.
/// Test: `reap_interval_secs_*` unit tests below.
pub fn reap_interval_secs() -> Option<u64> {
    match std::env::var(REAP_INTERVAL_ENV) {
        Ok(v) => match v.trim().parse::<u64>() {
            Ok(0) => None,
            Ok(n) => Some(n),
            Err(_) => Some(DEFAULT_REAP_INTERVAL_SECS),
        },
        Err(_) => Some(DEFAULT_REAP_INTERVAL_SECS),
    }
}

/// True iff `root_path` is a safely-reapable orphan.
///
/// Why: distinguishing a genuinely-deleted project directory from a transiently
/// unavailable one is the whole safety story, and there are two failure modes.
/// (1) A temporarily-unmounted volume (`/Volumes/Ext/project`) makes the root
/// vanish along with its parent — keying on "immediate parent still exists"
/// spares it while still reaping a deleted worktree
/// (`~/repo/.worktrees/<uuid>`), whose parent `.worktrees/` survives. (2) On
/// macOS a *mounted-but-TCC-blocked* external volume (issues #723 / #873, the
/// recurring FDA-loss-after-`cargo install` scenario) can make `exists()`
/// return `false` — or even hang — for a perfectly live root. So external
/// mounts under `/Volumes` are excluded outright *before any syscall*: the
/// reaper's target (MPM worktree churn) always lives under `$HOME`, never on a
/// removable volume, and manual `prune-orphans` still covers `/Volumes` when a
/// human confirms.
/// What: returns `false` for any `/Volumes/…` path (no stat performed) and for
/// paths that still exist; otherwise returns whether the immediate parent is a
/// non-empty, present directory.
/// Test: `is_reapable_orphan_*` unit tests below.
pub fn is_reapable_orphan(root_path: &Path) -> bool {
    // Cheap prefix check first — never stat (or risk hanging on) an external
    // volume that may be mounted-but-TCC-blocked.
    if root_path.starts_with("/Volumes") {
        return false;
    }
    if root_path.exists() {
        return false;
    }
    match root_path.parent() {
        Some(parent) => !parent.as_os_str().is_empty() && parent.exists(),
        None => false,
    }
}

/// Remove legacy registrations whose root was deleted, returning the survivors.
///
/// Why: warm-boot previously *detected* a missing non-colocated `root_path`,
/// logged "run `trusty-search prune-orphans`", and skipped it — leaving the
/// entry in `indexes.toml` to be re-read, re-warned, and re-skipped on every
/// subsequent boot forever. This converts that detection into action so the
/// registry self-heals across restarts.
/// What: partitions `entries` into reapable orphans (`!colocated` AND
/// [`is_reapable_orphan`]) and survivors. Colocated entries are deliberately
/// left alone so warm-boot's relocation scan can still recover a *moved* repo.
/// When any orphan is found, atomically rewrites `indexes.toml` to the
/// survivors (one write, matching the manual `prune-orphans` path and the
/// existing last-write-wins persistence contract), best-effort scrubs each
/// orphan's root from `roots.toml` so the colocated rescan cannot resurrect it,
/// and returns the survivors. On-disk index *data* is never touched.
/// Test: `heal_boot_orphans_*` unit tests below (partition logic); the save
/// path is shared with the `prune-orphans` CLI, whose tests cover it.
pub fn heal_boot_orphans(entries: Vec<PersistedIndex>) -> Vec<PersistedIndex> {
    let (orphans, kept) = partition_boot_orphans(entries);

    if orphans.is_empty() {
        return kept;
    }

    // #4317: remove BY ID rather than republishing `kept` wholesale. `entries`
    // was read before the daemon started serving, so a whole-file rewrite from
    // it erases any index registered in between — the reaper reporting a clean
    // self-heal while silently deregistering live indexes is exactly the
    // incident this issue tracks. Removing by id re-reads the current file under
    // the registry write lock and drops only what was judged orphaned.
    let orphan_ids: Vec<&str> = orphans.iter().map(|o| o.id.as_str()).collect();
    if let Err(e) = crate::service::persistence::remove_index_registry_entries(&orphan_ids) {
        tracing::warn!(
            "orphan-reaper(boot): could not rewrite indexes.toml ({e}); {} orphaned \
             registration(s) left on disk (they will be retried next boot)",
            orphans.len()
        );
        // Still return `kept` so this boot does not attempt to restore the dead
        // entries; only the on-disk cleanup was skipped.
        return kept;
    }

    for orphan in &orphans {
        if let Err(e) = crate::service::roots_registry::remove_root(&orphan.root_path) {
            tracing::debug!(
                "orphan-reaper(boot): could not remove root {} from roots.toml: {e}",
                orphan.root_path.display()
            );
        }
    }

    tracing::info!(
        "orphan-reaper(boot): removed {} orphaned registration(s) whose root_path was \
         deleted (self-heal); {} registration(s) remain",
        orphans.len(),
        kept.len()
    );

    kept
}

/// Pure split of registry entries into reapable orphans and survivors.
///
/// Why: extracted from [`heal_boot_orphans`] so the reap decision can be
/// unit-tested hermetically, without touching the real `indexes.toml`.
/// What: partitions on `!colocated && is_reapable_orphan(root_path)` — the
/// first `Vec` is the orphans to drop, the second the survivors to keep.
/// Test: `heal_boot_orphans_*` unit tests below.
pub(crate) fn partition_boot_orphans(
    entries: Vec<PersistedIndex>,
) -> (Vec<PersistedIndex>, Vec<PersistedIndex>) {
    entries
        .into_iter()
        .partition(|e| !e.colocated && is_reapable_orphan(&e.root_path))
}

/// Environment variable overriding how long an ambiguous-root deferral is
/// tolerated before its *registration* is reaped, in seconds (#4095).
/// `0` disables the terminal path entirely (defer forever, still warned).
pub const AMBIGUOUS_ROOT_GRACE_ENV: &str = "TRUSTY_AMBIGUOUS_ROOT_GRACE_SECS";

/// Default grace period for an ambiguous-root deferral: 7 days (#4095).
///
/// Why: the deferral exists because the daemon genuinely cannot tell which of
/// N candidate roots owns the index — auto-picking would relink an index to the
/// wrong project's data. So the grace window must be long enough for a human to
/// notice and run the documented `trusty-search index <path>` fix, and the
/// terminal action must be conservative. Seven days is far longer than any
/// legitimate relocation takes and far shorter than the 8-week accumulation the
/// incident showed.
const DEFAULT_AMBIGUOUS_ROOT_GRACE_SECS: u64 = 7 * 24 * 3600;

/// Resolve the ambiguous-root grace period from the environment (#4095).
///
/// Why/What/Test: mirrors [`reap_interval_secs`] exactly — `None` when the var
/// is `0` (terminal reap disabled), otherwise a positive value or the default.
/// Test: `ambiguous_root_grace_secs_env_branches`.
pub fn ambiguous_root_grace_secs() -> Option<u64> {
    match std::env::var(AMBIGUOUS_ROOT_GRACE_ENV) {
        Ok(v) => match v.trim().parse::<u64>() {
            Ok(0) => None,
            Ok(n) => Some(n),
            Err(_) => Some(DEFAULT_AMBIGUOUS_ROOT_GRACE_SECS),
        },
        Err(_) => Some(DEFAULT_AMBIGUOUS_ROOT_GRACE_SECS),
    }
}

/// What the reaper should do about an entry deferred on ambiguous candidates
/// (#4095).
///
/// Why: the previous behaviour had exactly one outcome — defer, silently,
/// forever — so the decision had no name and no test. Naming the three
/// outcomes makes the terminal path reviewable and lets the age arithmetic be
/// unit-tested without touching `indexes.toml` or a clock.
/// What: a pure decision. [`Self::ReapRegistration`] removes the registry row
/// ONLY — on-disk index data is never deleted by any variant, matching the
/// module-level safety contract.
/// Test: `classify_ambiguous_root_*` unit tests below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AmbiguousRootAction {
    /// First ambiguous observation for this entry — record `now` and warn.
    Stamp,
    /// Already stamped and still inside the grace window (or the terminal path
    /// is disabled) — warn with the accumulated age, do not act.
    KeepWaiting { age_secs: u64 },
    /// Grace window exhausted — drop the *registration* so the debris stops
    /// degrading health. On-disk index data is left untouched, so the fix is
    /// still `trusty-search index <path>` against the correct root.
    ReapRegistration { age_secs: u64 },
}

/// Decide what to do about an ambiguous-root deferral (#4095).
///
/// Why: the reaper deferred on ambiguity and never revisited, so every such
/// entry became permanent registry debris that pinned `search_health` to
/// `degraded` and masked real signals. This gives the deferral a terminal path
/// — but a *conservative* one: crossing the threshold removes the registration,
/// never the index data, because silently deleting a corpus is a strictly worse
/// failure than the debris it would clean up.
/// What: pure. `None` first-seen → [`AmbiguousRootAction::Stamp`]. A stamp
/// inside the grace window, a `grace` of `None` (terminal path disabled), or a
/// clock that went backwards → [`AmbiguousRootAction::KeepWaiting`]. Otherwise
/// [`AmbiguousRootAction::ReapRegistration`].
/// Test: `classify_ambiguous_root_stamps_on_first_sight`,
/// `classify_ambiguous_root_keeps_waiting_inside_grace`,
/// `classify_ambiguous_root_reaps_after_grace`,
/// `classify_ambiguous_root_never_reaps_when_grace_disabled`,
/// `classify_ambiguous_root_tolerates_clock_skew`.
pub fn classify_ambiguous_root(
    first_seen_unix: Option<u64>,
    now_unix: u64,
    grace_secs: Option<u64>,
) -> AmbiguousRootAction {
    let Some(first_seen) = first_seen_unix else {
        return AmbiguousRootAction::Stamp;
    };
    // `saturating_sub` also covers a clock that jumped backwards (NTP step,
    // a restored snapshot): age reads as 0, so we wait rather than reap on a
    // bogus elapsed time.
    let age_secs = now_unix.saturating_sub(first_seen);
    match grace_secs {
        Some(grace) if age_secs >= grace => AmbiguousRootAction::ReapRegistration { age_secs },
        _ => AmbiguousRootAction::KeepWaiting { age_secs },
    }
}

/// Render a duration for an operator-facing log line (#4095 review follow-up).
///
/// Why: the reaper's messages divided by 86,400 to print days, so every
/// sub-day duration rendered as a flat "0 day(s)" — including the default test
/// grace and, most misleadingly, a DISABLED terminal path. A log line that
/// says "removed in 0 days" when removal is off is worse than no line at all.
/// What: picks the largest unit that yields a non-zero value (day → hour →
/// minute → second) and pluralises it. Deliberately coarse: this is a human
/// hint, not a precise countdown.
/// Test: `render_duration_secs_picks_a_sensible_unit`.
fn render_duration_secs(secs: u64) -> String {
    const MINUTE: u64 = 60;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;
    let (value, unit) = if secs >= DAY {
        (secs / DAY, "day")
    } else if secs >= HOUR {
        (secs / HOUR, "hour")
    } else if secs >= MINUTE {
        (secs / MINUTE, "minute")
    } else {
        (secs, "second")
    };
    format!("{value} {unit}{}", if value == 1 { "" } else { "s" })
}

/// Apply [`classify_ambiguous_root`] to one entry, persisting and logging
/// (#4095).
///
/// Why: the relocation scan's ambiguous branch logged at WARN and returned —
/// and in this daemon only ERROR-level events reach `errors.jsonl` /
/// `list_recent_errors` / `tm doctor`, so the deferral was invisible on every
/// diagnostic surface an operator actually reads. Both the stamp and the
/// terminal reap are therefore logged at ERROR; the in-window wait stays WARN
/// so a long grace period does not spam the error buffer once per boot.
/// What: reads the clock, classifies, then either stamps
/// `ambiguous_root_since_unix` into `indexes.toml`, waits, or removes the
/// registry row (and scrubs the root from `roots.toml` so the colocated rescan
/// cannot resurrect it). **Never deletes on-disk index data** — `remove_index_data_dir`
/// is deliberately not called here; the corpus outlives the registration so a
/// mis-fired reap is fully recoverable by re-registering.
/// Test: `classify_ambiguous_root_*` cover the decision; this thin IO wrapper
/// reuses `remove_index_registry_entry` / `upsert_index_registry_entry`, both
/// already covered by the `prune-orphans` and dedup-self-heal paths.
pub fn handle_ambiguous_root(entry: &PersistedIndex, candidate_count: usize) {
    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let grace = ambiguous_root_grace_secs();
    match classify_ambiguous_root(entry.ambiguous_root_since_unix, now_unix, grace) {
        AmbiguousRootAction::Stamp => {
            let stamped = PersistedIndex {
                ambiguous_root_since_unix: Some(now_unix),
                ..entry.clone()
            };
            // #4095 review follow-up: the ERROR below used to fire even when
            // this upsert failed, telling an operator a deferral clock had
            // started when nothing had been persisted — a log claiming state
            // the system does not have, which is the exact failure mode this
            // issue is about. Report what actually happened instead.
            let persisted =
                crate::service::persistence::upsert_index_registry_entry(stamped).is_ok();
            // #4095 review follow-up: `grace / 86_400` rendered "0 day(s)" for
            // any sub-day grace, and — worse — rendered "removed in 0 day(s)"
            // when the terminal path was DISABLED (`grace == None`), i.e. the
            // exact opposite of the truth.
            let deadline = match grace {
                Some(g) => format!(
                    "If it is still unresolved after {}, the REGISTRATION will be removed \
                     automatically (on-disk index data is never deleted); set {}=0 to disable \
                     that.",
                    render_duration_secs(g),
                    AMBIGUOUS_ROOT_GRACE_ENV,
                ),
                None => format!(
                    "Automatic removal is DISABLED ({}=0), so this entry will accumulate as \
                     registry debris until you fix it.",
                    AMBIGUOUS_ROOT_GRACE_ENV,
                ),
            };
            let clock = if persisted {
                "The deferral clock starts now."
            } else {
                "WARNING: the deferral clock could NOT be persisted to indexes.toml, so it \
                 restarts on the next boot and the grace period will not elapse."
            };
            tracing::error!(
                index_id = %entry.id,
                candidates = candidate_count,
                stamp_persisted = persisted,
                "orphan-reaper(ambiguous): index '{}' root {} no longer exists and {} \
                 ambiguous relocation candidates were found, so the daemon cannot safely \
                 pick one. DEFERRED — fix it with `trusty-search index <path>` against the \
                 correct root. {} {} (issue #4095)",
                entry.id,
                entry.root_path.display(),
                candidate_count,
                clock,
                deadline,
            );
        }
        AmbiguousRootAction::KeepWaiting { age_secs } => {
            tracing::warn!(
                index_id = %entry.id,
                candidates = candidate_count,
                age_secs,
                "orphan-reaper(ambiguous): index '{}' still deferred after {} — {} \
                 ambiguous candidates for missing root {}. Fix with \
                 `trusty-search index <path>`. (issue #4095)",
                entry.id,
                render_duration_secs(age_secs),
                candidate_count,
                entry.root_path.display(),
            );
        }
        AmbiguousRootAction::ReapRegistration { age_secs } => {
            // Registration only. `remove_index_data_dir` is NOT called: the
            // whole point of the grace window is that we still do not know
            // which root owns this index, and destroying a corpus we cannot
            // identify would be a far worse failure than the debris.
            if let Err(e) = crate::service::persistence::remove_index_registry_entry(&entry.id) {
                tracing::warn!(
                    "orphan-reaper(ambiguous): could not remove indexes.toml row for '{}': {e} \
                     (issue #4095; will retry next boot)",
                    entry.id
                );
                return;
            }
            if let Err(e) = crate::service::roots_registry::remove_root(&entry.root_path) {
                tracing::debug!(
                    "orphan-reaper(ambiguous): could not remove root {} from roots.toml: {e}",
                    entry.root_path.display()
                );
            }
            tracing::error!(
                index_id = %entry.id,
                candidates = candidate_count,
                age_secs,
                "orphan-reaper(ambiguous): REMOVED the registration for index '{}' — its root \
                 {} has been missing with {} ambiguous relocation candidates for {}, past the \
                 {} grace period. ON-DISK INDEX DATA WAS NOT DELETED: only the \
                 `indexes.toml` row and the `roots.toml` entry were removed, so re-register \
                 with `trusty-search index <path>` to recover it. (issue #4095)",
                entry.id,
                entry.root_path.display(),
                candidate_count,
                render_duration_secs(age_secs),
                // `classify_ambiguous_root` only ever returns this variant when
                // `grace` is `Some`, so the fallback is unreachable in practice.
                render_duration_secs(grace.unwrap_or_default()),
            );
        }
    }
}

/// Clear a stale ambiguity stamp once an entry resolves cleanly again (#4095).
///
/// Why: without this the grace clock would keep running across a transient
/// ambiguity (two candidates during a migration, one afterwards) and eventually
/// reap a registration that has been healthy for weeks.
/// What: no-op when no stamp is set (the overwhelmingly common path, so this
/// costs one `Option` check per restore). Otherwise rewrites the row with the
/// stamp cleared.
/// Test: covered by `ambiguity_stamp_is_cleared_when_root_resolves` in
/// `service::server::tests_4087`'s sibling `#4095` coverage.
pub fn clear_ambiguous_root_stamp(entry: &PersistedIndex) {
    if entry.ambiguous_root_since_unix.is_none() {
        return;
    }
    let cleared = PersistedIndex {
        ambiguous_root_since_unix: None,
        ..entry.clone()
    };
    if let Err(e) = crate::service::persistence::upsert_index_registry_entry(cleared) {
        tracing::warn!(
            "orphan-reaper(ambiguous): could not clear deferral stamp for '{}': {e} \
             (issue #4095)",
            entry.id
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn entry(id: &str, root: PathBuf, colocated: bool) -> PersistedIndex {
        PersistedIndex {
            id: id.to_string(),
            root_path: root,
            colocated,
            ..Default::default()
        }
    }

    /// Why: a live directory must never be treated as an orphan.
    /// What: the tempdir itself exists → not reapable.
    /// Test: this test.
    #[test]
    fn is_reapable_orphan_false_for_live_root() {
        let tmp = tempdir().unwrap();
        assert!(!is_reapable_orphan(tmp.path()));
    }

    /// Why: the core self-heal case — a deleted leaf whose parent survives (the
    /// vanished-worktree shape) must be reapable.
    /// What: point at a non-existent child of an existing tempdir.
    /// Test: this test.
    #[test]
    fn is_reapable_orphan_true_for_deleted_leaf_with_live_parent() {
        let tmp = tempdir().unwrap();
        let dead = tmp.path().join("worktree-abc123");
        assert!(!dead.exists());
        assert!(is_reapable_orphan(&dead));
    }

    /// Why: the safety guarantee — an unmounted volume (whole parent chain gone)
    /// must NOT be reaped, or a remount would find its registration destroyed.
    /// What: a path whose parent also does not exist is not reapable.
    /// Test: this test.
    #[test]
    fn is_reapable_orphan_false_when_parent_also_missing() {
        let missing = PathBuf::from("/no/such/mount/point/project");
        assert!(!missing.exists());
        assert!(!is_reapable_orphan(&missing));
    }

    /// Why: external `/Volumes/…` mounts are excluded outright — a mounted-but-
    /// TCC-blocked or unmounted volume must never be auto-reaped (issues
    /// #723/#873), even when its parent `/Volumes` exists.
    /// What: a non-existent `/Volumes/Ext/project` is not reapable despite
    /// `/Volumes` being present.
    /// Test: this test.
    #[test]
    fn is_reapable_orphan_false_for_external_volume() {
        let vol = PathBuf::from("/Volumes/DefinitelyNotMounted-xyz/project");
        assert!(!vol.exists());
        assert!(!is_reapable_orphan(&vol));
    }

    /// Why: `0` disables the reaper; anything else parses or falls back.
    /// What: exercises the three branches via a scoped env guard.
    /// Test: this test.
    #[test]
    fn reap_interval_secs_env_branches() {
        // Serialize on a process-global to avoid cross-test env races.
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var(REAP_INTERVAL_ENV, "0");
        assert_eq!(reap_interval_secs(), None);
        std::env::set_var(REAP_INTERVAL_ENV, "120");
        assert_eq!(reap_interval_secs(), Some(120));
        std::env::set_var(REAP_INTERVAL_ENV, "not-a-number");
        assert_eq!(reap_interval_secs(), Some(DEFAULT_REAP_INTERVAL_SECS));
        std::env::remove_var(REAP_INTERVAL_ENV);
        assert_eq!(reap_interval_secs(), Some(DEFAULT_REAP_INTERVAL_SECS));
    }

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Why: a colocated orphan must be preserved so warm-boot's relocation scan
    /// can still recover a moved repo; only non-colocated dead roots are reaped.
    /// What: two dead-leaf entries (one colocated) plus one live entry → only
    /// the non-colocated dead one is dropped.
    /// Test: this test.
    #[test]
    fn heal_boot_orphans_reaps_only_non_colocated_dead_roots() {
        let tmp = tempdir().unwrap();
        let dead_legacy = tmp.path().join("gone-legacy");
        let dead_colocated = tmp.path().join("gone-colocated");
        let live = tmp.path().to_path_buf();

        let (orphans, kept) = partition_boot_orphans(vec![
            entry("legacy", dead_legacy, false),
            entry("colocated", dead_colocated, true),
            entry("live", live, false),
        ]);

        let orphan_ids: Vec<&str> = orphans.iter().map(|e| e.id.as_str()).collect();
        let kept_ids: Vec<&str> = kept.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(
            orphan_ids,
            ["legacy"],
            "only the dead legacy root is reaped"
        );
        assert!(
            kept_ids.contains(&"colocated"),
            "colocated orphan must be preserved for relocation"
        );
        assert!(kept_ids.contains(&"live"), "live root must be preserved");
    }

    const DAY: u64 = 86_400;

    /// Why (issue #4095): the very first ambiguous observation had no record
    /// at all, which is exactly why the deferral could never age out. It must
    /// start a clock.
    /// Test: this test.
    #[test]
    fn classify_ambiguous_root_stamps_on_first_sight() {
        assert_eq!(
            classify_ambiguous_root(None, 1_000_000, Some(7 * DAY)),
            AmbiguousRootAction::Stamp
        );
    }

    /// Why: the grace window is the whole safety story — an entry inside it
    /// must NOT be reaped, however loudly it is warned about.
    /// Test: this test.
    #[test]
    fn classify_ambiguous_root_keeps_waiting_inside_grace() {
        let first = 1_000_000;
        let action = classify_ambiguous_root(Some(first), first + 6 * DAY, Some(7 * DAY));
        assert_eq!(
            action,
            AmbiguousRootAction::KeepWaiting { age_secs: 6 * DAY }
        );
    }

    /// Why (issue #4095, the terminal path): past the threshold the entry must
    /// finally leave the registry, or the debris accumulates forever.
    /// What: also pins the boundary as inclusive (`age >= grace`).
    /// Test: this test.
    #[test]
    fn classify_ambiguous_root_reaps_after_grace() {
        let first = 1_000_000;
        assert_eq!(
            classify_ambiguous_root(Some(first), first + 7 * DAY, Some(7 * DAY)),
            AmbiguousRootAction::ReapRegistration { age_secs: 7 * DAY },
            "the boundary is inclusive"
        );
        assert_eq!(
            classify_ambiguous_root(Some(first), first + 30 * DAY, Some(7 * DAY)),
            AmbiguousRootAction::ReapRegistration { age_secs: 30 * DAY }
        );
    }

    /// Why: an operator must be able to switch the terminal path off entirely
    /// (`TRUSTY_AMBIGUOUS_ROOT_GRACE_SECS=0`) and get the pre-#4095 behaviour
    /// plus the new warnings — never an automatic removal.
    /// Test: this test.
    #[test]
    fn classify_ambiguous_root_never_reaps_when_grace_disabled() {
        let first = 1_000_000;
        assert_eq!(
            classify_ambiguous_root(Some(first), first + 3650 * DAY, None),
            AmbiguousRootAction::KeepWaiting {
                age_secs: 3650 * DAY
            },
            "grace=None must never reap, no matter the age"
        );
    }

    /// Why: an NTP step or a restored snapshot can move the clock backwards.
    /// Computing a wrapped age there would reap instantly — the exact silent
    /// data-affecting mistake this issue warns against.
    /// What: `now < first_seen` must read as age 0 and wait.
    /// Test: this test.
    #[test]
    fn classify_ambiguous_root_tolerates_clock_skew() {
        assert_eq!(
            classify_ambiguous_root(Some(2_000_000), 1_000_000, Some(7 * DAY)),
            AmbiguousRootAction::KeepWaiting { age_secs: 0 }
        );
    }

    /// Why: the grace knob must honour `0` (disabled) and fall back safely,
    /// mirroring `reap_interval_secs`.
    /// Test: this test.
    #[test]
    fn ambiguous_root_grace_secs_env_branches() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var(AMBIGUOUS_ROOT_GRACE_ENV, "0");
        assert_eq!(ambiguous_root_grace_secs(), None);
        std::env::set_var(AMBIGUOUS_ROOT_GRACE_ENV, "600");
        assert_eq!(ambiguous_root_grace_secs(), Some(600));
        std::env::set_var(AMBIGUOUS_ROOT_GRACE_ENV, "garbage");
        assert_eq!(
            ambiguous_root_grace_secs(),
            Some(DEFAULT_AMBIGUOUS_ROOT_GRACE_SECS)
        );
        std::env::remove_var(AMBIGUOUS_ROOT_GRACE_ENV);
        assert_eq!(
            ambiguous_root_grace_secs(),
            Some(DEFAULT_AMBIGUOUS_ROOT_GRACE_SECS)
        );
    }

    /// Why (issue #4095 safety contract): the terminal action is allowed to
    /// remove a *registration* and nothing else — deleting a corpus the daemon
    /// cannot even identify is strictly worse than the debris it would clean
    /// up. This pins that contract so a future data-deleting variant cannot be
    /// added without a reviewer confronting it.
    /// What: classifies every variant through an EXHAUSTIVE match with no
    /// wildcard arm. Adding a variant makes this test fail to COMPILE, which is
    /// the enforcement — a `format!("{:?}")` substring check (the first version
    /// of this test) proved nothing, since a `DeleteIndexData` variant would
    /// have passed it simply by being named something else.
    /// Test: this test.
    #[test]
    fn ambiguous_root_actions_never_include_data_deletion() {
        /// Does this action touch on-disk index DATA (as opposed to the
        /// registration)? Exhaustive by construction — no `_ =>` arm.
        fn deletes_index_data(action: AmbiguousRootAction) -> bool {
            match action {
                // Records a timestamp in indexes.toml.
                AmbiguousRootAction::Stamp => false,
                // Logs only.
                AmbiguousRootAction::KeepWaiting { .. } => false,
                // Removes the indexes.toml row + roots.toml entry. The corpus
                // on disk outlives it, which is what makes the reap reversible
                // via `trusty-search index <path>`.
                AmbiguousRootAction::ReapRegistration { .. } => false,
            }
        }

        for action in [
            AmbiguousRootAction::Stamp,
            AmbiguousRootAction::KeepWaiting { age_secs: 1 },
            AmbiguousRootAction::ReapRegistration { age_secs: 1 },
        ] {
            assert!(
                !deletes_index_data(action),
                "no ambiguous-root action may delete index data (issue #4095): {action:?}"
            );
        }
    }

    /// Why (#4095 review follow-up): every duration in the reaper's logs used
    /// integer-divide-by-86400, so a sub-day value printed "0 day(s)" — and a
    /// DISABLED terminal path printed "removed in 0 day(s)", the opposite of
    /// the truth.
    /// What: pins the unit selection and pluralisation across the boundaries.
    /// Test: this test.
    #[test]
    fn render_duration_secs_picks_a_sensible_unit() {
        assert_eq!(render_duration_secs(7 * DAY), "7 days");
        assert_eq!(render_duration_secs(DAY), "1 day");
        assert_eq!(render_duration_secs(2 * 3600), "2 hours");
        assert_eq!(render_duration_secs(600), "10 minutes");
        assert_eq!(render_duration_secs(45), "45 seconds");
        assert_eq!(render_duration_secs(1), "1 second");
        // The regression: a sub-day grace must never collapse to "0 days".
        assert!(!render_duration_secs(600).starts_with('0'));
    }

    /// Why: with no orphans the split must return everything as survivors.
    /// What: all-live input yields zero orphans.
    /// Test: this test.
    #[test]
    fn heal_boot_orphans_noop_when_all_live() {
        let tmp = tempdir().unwrap();
        let (orphans, kept) =
            partition_boot_orphans(vec![entry("a", tmp.path().to_path_buf(), false)]);
        assert!(orphans.is_empty());
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].id, "a");
    }
}
