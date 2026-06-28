//! Handler for `trusty-search prune` (issue #1782).
//!
//! Why: trusty-search accumulates indexes for projects that are no longer
//! actively searched. These stale indexes waste disk (HNSW snapshot + redb
//! corpus) and slow warm-boot. A dedicated `prune` command lets operators
//! review and delete indexes that have been idle for longer than a configurable
//! retention window, without needing the daemon to be running.
//!
//! What: reads `indexes.toml` (offline, no daemon required), computes each
//! index's idle age from `last_queried_unix` (falling back to
//! `last_indexed_unix`), and classifies each entry as Eligible / Protected /
//! NotTracked / Recent. Default mode is DRY-RUN (print only). `--apply`
//! deletes eligible, non-protected indexes via offline deletion helpers.
//!
//! Safety contract:
//! - Dry-run is the default — `--apply` must be explicit.
//! - Protected indexes (listed in `auto_prune.protected_indexes` config) are
//!   NEVER deleted, even with `--apply`.
//! - Indexes with no timestamp (`last_queried_unix = None` AND
//!   `last_indexed_unix = None`) are NOT eligible — they may have been freshly
//!   created or belong to a pre-tracking installation; deleting them would be
//!   surprising.
//! - The `auto_prune.enabled` config flag governs the future daemon-side
//!   scheduled sweep. The manual `prune --apply` command does NOT check this
//!   flag — explicit `--apply` is sufficient operator intent.
//! - Malformed config (YAML parse error): propagated as an error when `--apply`
//!   is active so that a corrupt config cannot silently empty `protected_indexes`
//!   and delete something that should be protected. Dry-runs warn but proceed.
//! - `--max-idle-days` must be at least 1; 0 would make every tracked index
//!   eligible and is rejected as a footgun.
//! - Stop the daemon before `--apply` to avoid a race between the daemon's
//!   periodic `last_queried_unix` flush and the registry save here.
//! - Dry-run (no `--apply`) performs NO filesystem mutations of any kind.
//!
//! Persistence approach: `last_queried_unix` is already written to
//! `indexes.toml` by the daemon search handler (rate-limited to at most once
//! per 60 s). The prune command reads the TOML file directly — it is fully
//! offline. Timestamp granularity is seconds, with a 60-second write
//! coarsening inside the daemon; the effective per-index resolution for prune
//! decisions is therefore "last day searched", which matches the day-scale
//! `max_idle_days` threshold.
//!
//! TODO (issue #1782 follow-up): daemon-side scheduled sweep that runs
//! periodically when `auto_prune.enabled = true`. The manual command is the
//! must-have for this PR.
//!
//! Test: `prune_tests` module (`src/commands/prune_tests.rs`).

use anyhow::{Context, Result};
use colored::Colorize;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::{AutoPruneConfig, GlobalConfig};
use crate::service::colocated_storage::COLOCATED_DIR_NAME;
use crate::service::persistence::{
    indexes_toml_path, load_index_registry_at, remove_index_data_dir, sanitize_id_for_path,
    save_index_registry_at, PersistedIndex,
};

/// Decision for one index entry (computed by [`classify_entry`]).
///
/// Why: keeping the classification pure lets unit tests verify eligibility
///      without touching the filesystem.
/// What: each variant carries metadata needed for the report / deletion.
/// Test: covered by eligibility unit tests in `prune_tests`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PruneDecision {
    /// Index is idle for longer than `max_idle_days` and not protected.
    Eligible {
        /// Days since the last query (or last index if never queried).
        idle_days: u64,
        /// On-disk footprint in bytes, or `None` when unknown.
        size_bytes: Option<u64>,
    },
    /// Index id is in `protected_indexes` — never delete.
    Protected,
    /// Both `last_queried_unix` and `last_indexed_unix` are absent.
    /// Safety: treated as ineligible to avoid surprising deletes on fresh
    /// installs or pre-tracking daemon versions.
    NotTracked,
    /// Index was queried or indexed more recently than `max_idle_days`.
    Recent {
        /// Days since last activity.
        idle_days: u64,
    },
}

/// Compute the idle-age classification for one registry entry.
///
/// Why: pure function so unit tests can drive eligibility logic without I/O.
/// What: picks `last_queried_unix` first, falls back to `last_indexed_unix`.
///       Returns `NotTracked` when both are absent. Otherwise computes elapsed
///       days and compares to `max_idle_days`. Protected entries always return
///       `Protected` regardless of age.
/// Test: `prune_eligibility_boundary`, `prune_not_tracked_is_not_eligible` in
///       `prune_tests`.
pub(crate) fn classify_entry(
    entry: &PersistedIndex,
    cfg: &AutoPruneConfig,
    now_unix: u64,
    size_fn: impl Fn(&str) -> Option<u64>,
) -> PruneDecision {
    // Check protection first — protected indexes are never eligible.
    if cfg.protected_indexes.iter().any(|p| p == &entry.id) {
        return PruneDecision::Protected;
    }

    // Pick the best available timestamp: last query > last index > absent.
    let last_activity = entry.last_queried_unix.or(entry.last_indexed_unix);

    let Some(ts) = last_activity else {
        return PruneDecision::NotTracked;
    };

    let elapsed_secs = now_unix.saturating_sub(ts);
    let idle_days = elapsed_secs / 86_400;

    if idle_days >= cfg.max_idle_days as u64 {
        let size_bytes = size_fn(&entry.id);
        PruneDecision::Eligible {
            idle_days,
            size_bytes,
        }
    } else {
        PruneDecision::Recent { idle_days }
    }
}

/// Format a byte count as a human-readable string (KB / MB / GB).
///
/// Why: raw byte counts are hard to read in a terminal table; operators
///      care about MB/GB-scale disk reclaim.
/// What: returns e.g. `"42.3 MB"`, `"1.2 GB"`, `"512 KB"`.
/// Test: `format_bytes_display_cases` in `prune_tests`.
pub(crate) fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1_024;
    const MB: u64 = 1_024 * KB;
    const GB: u64 = 1_024 * MB;
    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.0} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

/// Entry point for `trusty-search prune [--max-idle-days N] [--apply] [--yes]`.
///
/// Why: keep the CLI handler thin; delegates to `handle_prune_configured`
///      with no config-path override (uses the default platform path).
/// What: calls `handle_prune_configured(None, ...)`.
/// Test: covered by `handle_prune_configured` unit tests in `prune_tests`.
pub fn handle_prune(apply: bool, yes: bool, max_idle_days_override: Option<u32>) -> Result<()> {
    handle_prune_configured(None, apply, yes, max_idle_days_override)
}

/// Config-path-injectable entry point for `trusty-search prune`.
///
/// Why: `handle_prune` reads GlobalConfig from the default platform path,
///      which is not injectable in unit tests. This variant accepts an explicit
///      `config_path` so tests can exercise the malformed-config abort guard.
/// What: loads `GlobalConfig` (from `config_path` when `Some`, otherwise the
///       default path), aborts if config is malformed and `--apply` is active,
///       resolves the registry path, and delegates to `handle_prune_at`.
/// Test: `prune_malformed_config_aborts_apply` in `prune_tests`.
pub(crate) fn handle_prune_configured(
    config_path: Option<&Path>,
    apply: bool,
    yes: bool,
    max_idle_days_override: Option<u32>,
) -> Result<()> {
    // Load config FIRST so malformed config + --apply aborts before any
    // registry or filesystem access.
    let prune_cfg = load_prune_config(config_path, apply)?;

    let toml_path = indexes_toml_path()?;

    handle_prune_at(
        &toml_path,
        apply,
        yes,
        max_idle_days_override,
        prune_cfg,
        /*interactive=*/ true,
        /*size_fn=*/ default_size_fn,
        /*now_unix=*/ current_unix(),
    )
}

/// Load and validate the `auto_prune` section of `GlobalConfig`.
///
/// Why: extracted so both `handle_prune_configured` and tests share the same
///      error propagation / dry-run fallback logic.
/// What: loads from `config_path` (or default path when `None`). When config
///       is malformed and `apply=true`, propagates the error so no deletions can
///       proceed with silenced `protected_indexes`. When `apply=false`, warns and
///       proceeds with defaults.
/// Test: `prune_malformed_config_aborts_apply` in `prune_tests`.
fn load_prune_config(config_path: Option<&Path>, apply: bool) -> Result<AutoPruneConfig> {
    let result = match config_path {
        Some(p) => GlobalConfig::load_from(p),
        None => GlobalConfig::load(),
    };
    match result {
        Ok(cfg) => Ok(cfg.auto_prune),
        Err(e) => {
            if apply {
                Err(e).context(
                    "config.yaml is malformed — refusing --apply to protect \
                     indexes listed in auto_prune.protected_indexes. \
                     Fix config.yaml or remove the file to reset to defaults.",
                )
            } else {
                eprintln!(
                    "{} config.yaml could not be parsed ({}); proceeding with \
                     defaults for dry-run. Protected indexes may not be \
                     highlighted correctly.",
                    "warning:".yellow().bold(),
                    e
                );
                Ok(AutoPruneConfig::default())
            }
        }
    }
}

/// Path-injectable variant of the prune logic.
///
/// Why: unit tests need a tempfile registry without touching the user's real
///      `indexes.toml`. `interactive=false` skips the stdin confirmation
///      prompt. `size_fn` and `now_unix` are injected to keep tests
///      deterministic without real on-disk data or actual time. `prune_cfg`
///      is injected so tests can exercise protected_indexes without a real
///      GlobalConfig on disk.
/// What: validates `max_idle_days >= 1`, classifies every registry entry, prints
///       the report, optionally deletes eligible non-protected entries when
///       `apply=true` and confirmed. Registry removal is done as a single
///       batched save; on-disk data dirs are removed after the registry write.
/// Test: all `prune_*` unit tests in `prune_tests` call this variant.
#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_prune_at(
    toml_path: &Path,
    apply: bool,
    yes: bool,
    max_idle_days_override: Option<u32>,
    mut prune_cfg: AutoPruneConfig,
    interactive: bool,
    size_fn: impl Fn(&str) -> Option<u64>,
    now_unix: u64,
) -> Result<()> {
    // CLI override takes precedence over config.
    if let Some(days) = max_idle_days_override {
        prune_cfg.max_idle_days = days;
    }

    // Guard: 0 would make every tracked index eligible instantly.
    anyhow::ensure!(
        prune_cfg.max_idle_days >= 1,
        "--max-idle-days must be at least 1 (0 would make every tracked index eligible)"
    );

    let entries = load_index_registry_at(toml_path)?;

    if entries.is_empty() {
        println!("Registry is empty — nothing to prune.");
        return Ok(());
    }

    // Classify every entry.
    // Use owned PersistedIndex clones so the borrow on `entries` can be
    // released before the batch-save move below.
    let classifications: Vec<(PersistedIndex, PruneDecision)> = entries
        .iter()
        .map(|e| {
            let decision = classify_entry(e, &prune_cfg, now_unix, &size_fn);
            (e.clone(), decision)
        })
        .collect();

    // Collect eligible entries (those we WOULD delete with --apply).
    let eligible: Vec<(PersistedIndex, u64, Option<u64>)> = classifications
        .iter()
        .filter_map(|(e, d)| match d {
            PruneDecision::Eligible {
                idle_days,
                size_bytes,
            } => Some((e.clone(), *idle_days, *size_bytes)),
            _ => None,
        })
        .collect();

    let total_freeable: u64 = eligible.iter().filter_map(|(_, _, sz)| *sz).sum();

    // Print the report table (borrows into classifications).
    let class_refs: Vec<(&PersistedIndex, &PruneDecision)> =
        classifications.iter().map(|(e, d)| (e, d)).collect();
    print_report(&class_refs, &prune_cfg);

    if eligible.is_empty() {
        println!(
            "{} No indexes eligible for pruning (idle > {} days).",
            "✓".green(),
            prune_cfg.max_idle_days
        );
        return Ok(());
    }

    let size_hint = if total_freeable > 0 {
        format!(" (would free ~{})", format_bytes(total_freeable))
    } else {
        String::new()
    };

    if !apply {
        println!(
            "\n{} {} index(es) eligible{}. Re-run with {} to delete.",
            "ℹ".cyan(),
            eligible.len(),
            size_hint,
            "--apply".bold()
        );
        return Ok(());
    }

    // `--apply` path.
    println!(
        "\n{} {} index(es) eligible{}.",
        "!".yellow(),
        eligible.len(),
        size_hint
    );

    if !yes {
        if !interactive {
            println!("Aborted (non-interactive mode).");
            return Ok(());
        }
        if !super::confirm(&format!(
            "Permanently delete {} index(es) from the registry and disk?",
            eligible.len()
        ))? {
            println!("Aborted.");
            return Ok(());
        }
    }

    // Batch-remove eligible entries from the registry in one atomic save.
    // This prevents N separate load-retain-save cycles that race with the
    // daemon's last_queried_unix flush. (Stop the daemon before --apply for
    // a fully safe update.)
    let eligible_ids: Vec<&str> = eligible.iter().map(|(e, _, _)| e.id.as_str()).collect();
    let survivors: Vec<PersistedIndex> = entries
        .into_iter()
        .filter(|e| !eligible_ids.contains(&e.id.as_str()))
        .collect();
    let remaining = survivors.len();
    save_index_registry_at(toml_path, &survivors)?;

    // Delete on-disk data dirs after the registry save.
    let mut deleted = 0usize;
    let mut errors = 0usize;
    for (entry, idle_days, _) in &eligible {
        match remove_data_dir_for_entry(entry) {
            Ok(()) => {
                println!(
                    "  {} deleted {} (idle {} days)",
                    "−".red(),
                    entry.id.bold(),
                    idle_days
                );
                deleted += 1;
            }
            Err(e) => {
                eprintln!(
                    "{} failed to remove on-disk data for '{}': {e:#}",
                    "✗".red(),
                    entry.id
                );
                errors += 1;
            }
        }
    }

    // Summary.
    if errors == 0 {
        println!(
            "\n{} Deleted {} index(es). {} registration(s) remain.",
            "✓".green(),
            deleted,
            remaining
        );
    } else {
        println!(
            "\n{} Deleted {} index(es), {} failed (registry entries already removed). \
             {} registration(s) remain.",
            "⚠".yellow(),
            deleted,
            errors,
            remaining
        );
    }

    Ok(())
}

/// Delete on-disk data for one index, respecting colocated vs. global storage.
///
/// Why: colocated indexes store HNSW + redb under `<root_path>/.trusty-search/`,
///      not under the global `<data_dir>/indexes/<id>/`. Without this dispatch
///      a colocated deletion removes the registry entry but leaves the data on
///      disk. Crucially, we build the path WITHOUT calling `colocated_storage_dir`
///      (which unconditionally runs `create_dir_all`) to avoid ghost-dir creation
///      when the root_path no longer exists.
/// What: when `entry.colocated`, resolves `<root_path>/.trusty-search/` directly
///       via `entry.root_path.join(COLOCATED_DIR_NAME)`, guards `dir.exists()`,
///       then `remove_dir_all`. Otherwise calls `remove_index_data_dir`.
/// Test: `prune_colocated_deletion_removes_colocated_dir` and
///       `prune_colocated_absent_root_creates_no_dirs` in `prune_tests`.
fn remove_data_dir_for_entry(entry: &PersistedIndex) -> Result<()> {
    if entry.colocated {
        // Build the path directly — no create_dir_all side effect.
        let dir: PathBuf = entry.root_path.join(COLOCATED_DIR_NAME);
        if dir.exists() {
            std::fs::remove_dir_all(&dir)
                .with_context(|| format!("remove colocated dir {}", dir.display()))?;
        }
        Ok(())
    } else {
        remove_index_data_dir(&entry.id)
    }
}

/// Print the classification table to stdout.
///
/// Why: extracted so the report logic can be read and verified independently
///      of the deletion logic.
/// What: prints one row per index entry with status (pre-padded before
///       colorization so ANSI escape bytes don't break column alignment),
///       id, idle age, and size.
/// Test: covered transitively by `handle_prune_at` tests.
fn print_report(classifications: &[(&PersistedIndex, &PruneDecision)], cfg: &AutoPruneConfig) {
    let id_width = classifications
        .iter()
        .map(|(e, _)| e.id.len())
        .max()
        .unwrap_or(4)
        .max(4);

    println!(
        "  {:<width$}  {:<9}  {:<10}  {}",
        "INDEX".dimmed(),
        "STATUS".dimmed(),
        "IDLE DAYS".dimmed(),
        "DISK".dimmed(),
        width = id_width
    );

    for (entry, decision) in classifications {
        // Pad the raw label BEFORE colorizing so ANSI escape bytes don't
        // inflate the apparent width and misalign columns.
        let (status_str, idle_str, size_str): (String, String, String) = match decision {
            PruneDecision::Eligible {
                idle_days,
                size_bytes,
            } => (
                format!("{:<9}", "ELIGIBLE").red().to_string(),
                format!("{} days", idle_days),
                size_bytes.map(format_bytes).unwrap_or_else(|| "?".into()),
            ),
            PruneDecision::Protected => (
                format!("{:<9}", "PROTECTED").cyan().to_string(),
                "—".into(),
                "—".into(),
            ),
            PruneDecision::NotTracked => (
                format!("{:<9}", "UNTRACKED").yellow().to_string(),
                "unknown".into(),
                "—".into(),
            ),
            PruneDecision::Recent { idle_days } => (
                format!("{:<9}", "RECENT").green().to_string(),
                format!("{} days", idle_days),
                "—".into(),
            ),
        };

        println!(
            "  {:<width$}  {}  {:<10}  {}",
            entry.id.bold(),
            status_str,
            idle_str,
            size_str,
            width = id_width
        );
    }
    println!(
        "\n  Threshold: {} days ({})",
        cfg.max_idle_days,
        if cfg.enabled {
            "auto-prune sweep enabled".green().to_string()
        } else {
            "auto-prune sweep disabled — use --apply for manual deletion"
                .dimmed()
                .to_string()
        }
    );
}

/// Read the current Unix timestamp.
///
/// Why: injecting `now_unix` into `handle_prune_at` makes tests deterministic.
/// What: `SystemTime::now()` as seconds since UNIX_EPOCH, saturating to 0 on error.
/// Test: covered transitively.
fn current_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Compute the base data directory path WITHOUT calling `create_dir_all`.
///
/// Why: `data_dir()` in `persistence.rs` calls `create_dir_all` — using it in
///      `default_size_fn` would mutate the filesystem during a dry-run. This
///      function derives the same canonical path without any side effects.
/// What: honours `TRUSTY_DATA_DIR` env-override (absolute path required), then
///       falls back to `dirs::data_local_dir().join("trusty-search")`. Returns
///       `None` when neither source yields a path.
/// Test: side-effect-free; exercised transitively via `default_size_fn`.
fn data_dir_no_create() -> Option<PathBuf> {
    if let Ok(override_dir) = std::env::var("TRUSTY_DATA_DIR") {
        let dir = PathBuf::from(override_dir);
        if dir.is_absolute() {
            return Some(dir);
        }
    }
    dirs::data_local_dir().map(|b| b.join("trusty-search"))
}

/// Default `size_fn`: returns the on-disk byte count for the index data dir.
///
/// Why: production callers need real disk sizes; test callers inject a stub.
/// What: builds the path as `<data_dir_no_create>/indexes/<sanitized_id>`
///       without any `create_dir_all` side effect. Falls back to `None` when
///       the dir does not exist.
/// Test: covered transitively in production; test callers inject `no_size`.
fn default_size_fn(id: &str) -> Option<u64> {
    let dir = data_dir_no_create()?
        .join("indexes")
        .join(sanitize_id_for_path(id));
    if dir.exists() {
        Some(trusty_common::sys_metrics::dir_size_bytes(&dir))
    } else {
        None
    }
}

#[cfg(test)]
#[path = "prune_tests.rs"]
mod tests;
