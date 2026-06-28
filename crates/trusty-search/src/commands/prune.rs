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
//! - The `auto_prune.enabled` config flag governs the daemon-side scheduled
//!   sweep. The manual `prune --apply` command does NOT require `enabled=true`
//!   — explicit use of `--apply` is sufficient operator intent. Set `enabled`
//!   to opt in to future daemon-driven automatic deletion.
//! - Malformed config (YAML parse error): propagated as an error when `--apply`
//!   is active so that a corrupt config cannot silently empty `protected_indexes`
//!   and delete something that should be protected. Dry-runs warn but proceed.
//! - Stop the daemon before `--apply` to avoid a race between the daemon's
//!   periodic `last_queried_unix` flush and the registry save here.
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
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::{AutoPruneConfig, GlobalConfig};
use crate::service::persistence::{
    data_dir, indexes_toml_path, load_index_registry_at, remove_index_data_dir,
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
/// Why: keep the CLI handler thin; all logic in `handle_prune_at` which is
///      testable with injected config / path / size_fn / now_unix.
/// What: loads `GlobalConfig`, applies CLI overrides, delegates to
///       `handle_prune_at`. When `--apply` is active a malformed config is a
///       hard error (so corrupt config cannot silence `protected_indexes`).
///       A dry-run emits a warning and proceeds with defaults.
/// Test: covered by `handle_prune_at` unit tests in `prune_tests`.
pub fn handle_prune(apply: bool, yes: bool, max_idle_days_override: Option<u32>) -> Result<()> {
    let toml_path = indexes_toml_path()?;

    // Load global config for prune settings.
    let prune_cfg = match GlobalConfig::load() {
        Ok(cfg) => cfg.auto_prune,
        Err(e) => {
            if apply {
                // CRITICAL: malformed config when --apply is active could
                // silently clear protected_indexes and delete protected data.
                return Err(e).context(
                    "config.yaml is malformed — refusing --apply to protect \
                     indexes listed in auto_prune.protected_indexes. \
                     Fix config.yaml or remove the file to reset to defaults.",
                );
            }
            // Dry-run: warn and proceed with defaults (no deletions happen).
            eprintln!(
                "{} config.yaml could not be parsed ({}); proceeding with defaults \
                 for dry-run. Protected indexes may not be highlighted correctly.",
                "warning:".yellow().bold(),
                e
            );
            AutoPruneConfig::default()
        }
    };

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

/// Path-injectable variant of [`handle_prune`].
///
/// Why: unit tests need a tempfile registry without touching the user's real
///      `indexes.toml`. `interactive=false` skips the stdin confirmation
///      prompt. `size_fn` and `now_unix` are injected to keep tests
///      deterministic without real on-disk data or actual time. `prune_cfg`
///      is injected so tests can exercise protected_indexes without a real
///      GlobalConfig on disk.
/// What: classifies every registry entry, prints the report, optionally deletes
///       eligible non-protected entries when `apply=true` and confirmed.
///       Registry removal is done as a single batched save; on-disk data dirs
///       are removed after the registry write.
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

    let entries = load_index_registry_at(toml_path)?;

    if entries.is_empty() {
        println!("Registry is empty — nothing to prune.");
        return Ok(());
    }

    // Classify every entry.
    // Use owned PersistedIndex clones in `classifications` so that the borrow
    // on `entries` can be released before the batch-save move.
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

    // Print the report table.
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
        let remove_result = remove_data_dir_for_entry(entry);
        match remove_result {
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
///      a colocated deletion would remove the registry entry and print "deleted"
///      while leaving the data orphaned on disk.
/// What: when `entry.colocated`, removes `<root_path>/.trusty-search/` via
///       `colocated_storage::colocated_storage_dir`; otherwise calls the
///       existing `remove_index_data_dir` helper.
/// Test: `prune_colocated_deletion_removes_colocated_dir` in `prune_tests`.
fn remove_data_dir_for_entry(entry: &PersistedIndex) -> Result<()> {
    if entry.colocated {
        let dir = crate::service::colocated_storage::colocated_storage_dir(&entry.root_path)?;
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
/// What: prints one row per index entry with status, id, idle age, and size.
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
        let (status_str, idle_str, size_str): (colored::ColoredString, String, String) =
            match decision {
                PruneDecision::Eligible {
                    idle_days,
                    size_bytes,
                } => (
                    "ELIGIBLE".red(),
                    format!("{} days", idle_days),
                    size_bytes.map(format_bytes).unwrap_or_else(|| "?".into()),
                ),
                PruneDecision::Protected => ("PROTECTED".cyan(), "—".into(), "—".into()),
                PruneDecision::NotTracked => ("UNTRACKED".yellow(), "unknown".into(), "—".into()),
                PruneDecision::Recent { idle_days } => {
                    ("RECENT".green(), format!("{} days", idle_days), "—".into())
                }
            };

        println!(
            "  {:<width$}  {:<9}  {:<10}  {}",
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

/// Default `size_fn`: returns the on-disk byte count for the index data dir.
///
/// Why: production callers need real disk sizes; test callers inject a stub.
/// What: builds the path as `<data_dir>/indexes/<sanitized_id>` WITHOUT calling
///       `index_data_dir` (which runs `create_dir_all` and would create empty
///       dirs during dry-run). Falls back to `None` when the dir does not exist.
/// Test: covered transitively in production; test callers inject `no_size`.
fn default_size_fn(id: &str) -> Option<u64> {
    // Replicate persistence::sanitize_id logic without calling index_data_dir
    // (which runs create_dir_all and would mutate the filesystem during dry-run).
    let safe_id: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let dir = data_dir().ok()?.join("indexes").join(safe_id);
    if dir.exists() {
        Some(trusty_common::sys_metrics::dir_size_bytes(&dir))
    } else {
        None
    }
}

#[cfg(test)]
#[path = "prune_tests.rs"]
mod tests;
