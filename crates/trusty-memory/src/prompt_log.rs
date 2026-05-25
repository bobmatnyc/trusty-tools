//! Enriched-prompt logger for the UserPromptSubmit / SessionStart hooks
//! (issue #105).
//!
//! Why: `trusty-memory prompt-context` and `trusty-memory inbox-check` both
//! inject context into Claude Code sessions. Without a record of what was
//! injected we can't evaluate the effectiveness of either pipeline (relevance,
//! length, signal-vs-noise) or iterate on the recall / message-surfacing
//! logic. This module captures every invocation as a single JSONL entry under
//! the daemon data root so the logs are grep- and `jq`-friendly.
//!
//! What: a small, self-contained rolling writer. `PromptLogger::from_env`
//! reads the [`PromptLogConfig`] env vars, computes the active log path, and
//! returns a logger that swallows every I/O failure (best-effort by contract
//! — the hook caller must never fail because of a log write). The on-disk
//! layout is `<data_root>/logs/enriched-prompts.<YYYY-MM-DD>.jsonl` with a
//! `.<n>.jsonl` numeric suffix appended on size-cap rotation
//! (`enriched-prompts.2026-05-25.1.jsonl`, `.2.jsonl`, …).
//!
//! Rotation rules:
//!   - **Daily**: the date prefix in the filename changes when the local clock
//!     rolls over to a new UTC day.
//!   - **Size cap**: before each write, the active file's length is checked
//!     against `max_bytes` (default 50 MiB). When the cap would be exceeded
//!     the writer advances to the next numeric suffix.
//!
//! Retention: each successful first-write-of-the-day prunes files outside the
//! configured window (`retention_days`, default 30). The check is cheap (one
//! `read_dir` scan per first write per day).
//!
//! Privacy controls:
//!   - `TRUSTY_MEMORY_PROMPT_LOG=off` (or `0`, `false`, `no`, case-insensitive)
//!     disables the pipeline entirely — no files created, no I/O.
//!   - `TRUSTY_MEMORY_PROMPT_LOG_HASH_PROMPTS=1` (or `true`, `yes`, `on`)
//!     replaces the raw `trigger_prompt` with `sha256:<hex>` so the file holds
//!     no plaintext user input.
//!
//! Failure isolation: every public method swallows I/O / serialisation errors
//! and emits a `tracing::warn!` to stderr. The hook caller must never observe
//! a failure path from this module.
//!
//! Test: see [`tests`] for round-trip, rotation, retention, disabled, hash and
//! integration-style assertions.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Datelike, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Env var: master switch (`off`/`0`/`false`/`no` → disabled).
pub const ENV_ENABLED: &str = "TRUSTY_MEMORY_PROMPT_LOG";
/// Env var: directory override (defaults to `<data_root>/logs`).
pub const ENV_DIR: &str = "TRUSTY_MEMORY_PROMPT_LOG_DIR";
/// Env var: per-file size cap in bytes (default `DEFAULT_MAX_BYTES`).
pub const ENV_MAX_BYTES: &str = "TRUSTY_MEMORY_PROMPT_LOG_MAX_BYTES";
/// Env var: retention window in days (default `DEFAULT_RETENTION_DAYS`).
pub const ENV_RETENTION_DAYS: &str = "TRUSTY_MEMORY_PROMPT_LOG_RETENTION_DAYS";
/// Env var: SHA-256-hash `trigger_prompt` when truthy.
pub const ENV_HASH_PROMPTS: &str = "TRUSTY_MEMORY_PROMPT_LOG_HASH_PROMPTS";

/// Default per-file size cap (50 MiB).
pub const DEFAULT_MAX_BYTES: u64 = 50 * 1024 * 1024;
/// Default retention window in days.
pub const DEFAULT_RETENTION_DAYS: u32 = 30;
/// Filename stem prefix for log files.
const FILE_PREFIX: &str = "enriched-prompts";
/// Filename extension for log files.
const FILE_EXT: &str = "jsonl";

/// Configuration for [`PromptLogger`].
///
/// Why: keeps env-parsing out of the hot path and allows tests to construct
/// loggers directly without mutating process-wide env state. The struct is
/// `Clone` so a logger can be cheaply re-derived per invocation.
/// What: holds the resolved log directory, size cap, retention window, and
/// privacy toggles. `enabled = false` short-circuits every write.
/// Test: covered by `config_from_env_disabled` and the integration tests.
#[derive(Clone, Debug)]
pub struct PromptLogConfig {
    /// Master enable switch. `false` → every method is a no-op.
    pub enabled: bool,
    /// Directory holding the rolling log files (created lazily on first write).
    pub dir: PathBuf,
    /// Per-file size cap; the writer rolls to a new numeric suffix when the
    /// active file would exceed this size.
    pub max_bytes: u64,
    /// Retention window in days. Files older than this are pruned on the
    /// first write of each day.
    pub retention_days: u32,
    /// Replace `trigger_prompt` field bodies with `sha256:<hex>` when true.
    pub hash_prompts: bool,
}

impl PromptLogConfig {
    /// Build a config rooted at the supplied `data_root` and overlayed with
    /// env vars.
    ///
    /// Why: `prompt-context` and `inbox-check` both resolve their data root
    /// via [`trusty_common::resolve_data_dir`] but only that caller knows the
    /// app name. Accepting an explicit root lets the logger reuse the same
    /// resolution without parsing dirs::data_dir twice.
    /// What: defaults `dir = data_root/logs`; overrides via `TRUSTY_MEMORY_*`
    /// envs. `enabled` defaults to `true`; flips to `false` when
    /// `TRUSTY_MEMORY_PROMPT_LOG` is set to an off-value.
    /// Test: `config_from_env_defaults`, `config_from_env_disabled`,
    /// `config_from_env_overrides_dir`.
    pub fn from_env_with_root(data_root: &Path) -> Self {
        let enabled = match std::env::var(ENV_ENABLED) {
            Ok(v) => !is_off(&v),
            Err(_) => true,
        };
        let dir = match std::env::var(ENV_DIR) {
            Ok(d) if !d.trim().is_empty() => PathBuf::from(d),
            _ => data_root.join("logs"),
        };
        let max_bytes = std::env::var(ENV_MAX_BYTES)
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_MAX_BYTES);
        let retention_days = std::env::var(ENV_RETENTION_DAYS)
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_RETENTION_DAYS);
        let hash_prompts = std::env::var(ENV_HASH_PROMPTS)
            .map(|v| is_on(&v))
            .unwrap_or(false);
        Self {
            enabled,
            dir,
            max_bytes,
            retention_days,
            hash_prompts,
        }
    }
}

/// One enriched-prompt log entry — written as a single JSONL line.
///
/// Why: the consumer is a human running `jq` over a day's worth of injections
/// to grade signal-vs-noise. Stable field names, RFC-3339 timestamps, and
/// numeric byte/duration counts keep the analysis script trivial.
/// What: tagged by `injection_kind`. `palace_facts_count` is filled for
/// `prompt-context-facts`; `unread_messages_count` for `inbox-check-messages`.
/// Both default to `None` so the JSON shape stays compact for entries that
/// only have one of the two.
/// Test: `single_event_roundtrip` writes one entry and parses it back.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PromptLogEntry {
    /// RFC-3339 UTC timestamp set at the moment the entry is built.
    pub timestamp: DateTime<Utc>,
    /// `"UserPromptSubmit"` or `"SessionStart"`.
    pub hook_type: String,
    /// `"prompt-context-facts"` or `"inbox-check-messages"`.
    pub injection_kind: String,
    /// Palace id the injection was scoped to.
    pub palace: String,
    /// Hook stdin verbatim; replaced with `"sha256:<hex>"` when
    /// `hash_prompts = true` in the active config.
    pub trigger_prompt: String,
    /// Hook stdout (the actual injection sent to Claude Code) verbatim.
    pub injection: String,
    /// Byte length of `injection`.
    pub injection_length: usize,
    /// Number of facts in the prompt-context injection, when applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub palace_facts_count: Option<usize>,
    /// Number of unread messages in the inbox-check injection, when applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unread_messages_count: Option<usize>,
    /// Wall-clock duration of the invocation, in milliseconds.
    pub duration_ms: u64,
}

impl PromptLogEntry {
    /// Construct a new entry stamped with the current UTC time.
    ///
    /// Why: the hook caller has the raw fields handy but should not carry
    /// chrono in its imports. This helper builds an entry with `timestamp`
    /// auto-populated and zero-initialised optional counts.
    /// What: sets `timestamp = Utc::now()` and copies the supplied fields.
    /// Test: `single_event_roundtrip`.
    pub fn new(
        hook_type: impl Into<String>,
        injection_kind: impl Into<String>,
        palace: impl Into<String>,
        trigger_prompt: impl Into<String>,
        injection: impl Into<String>,
    ) -> Self {
        let injection = injection.into();
        let injection_length = injection.len();
        Self {
            timestamp: Utc::now(),
            hook_type: hook_type.into(),
            injection_kind: injection_kind.into(),
            palace: palace.into(),
            trigger_prompt: trigger_prompt.into(),
            injection,
            injection_length,
            palace_facts_count: None,
            unread_messages_count: None,
            duration_ms: 0,
        }
    }

    /// Builder: set the duration this hook invocation took.
    #[must_use]
    pub fn with_duration_ms(mut self, ms: u64) -> Self {
        self.duration_ms = ms;
        self
    }

    /// Builder: attach the palace-facts count (prompt-context only).
    #[must_use]
    pub fn with_palace_facts_count(mut self, n: usize) -> Self {
        self.palace_facts_count = Some(n);
        self
    }

    /// Builder: attach the unread-messages count (inbox-check only).
    #[must_use]
    pub fn with_unread_messages_count(mut self, n: usize) -> Self {
        self.unread_messages_count = Some(n);
        self
    }
}

/// Best-effort rolling JSONL writer.
///
/// Why: hook commands are short-lived (one entry per invocation), so the
/// logger is constructed at the start of the invocation, writes one line,
/// and drops at the end. There is no daemon path involved; cross-process
/// concurrency is handled by `OpenOptions::append(true)` which O_APPEND
/// atomically positions each write at end-of-file on POSIX. On Windows
/// (not a target for this crate) the same flag delivers similar guarantees
/// for writes under the 4 KiB pipe-atomicity threshold, which our JSONL
/// lines comfortably fit under.
/// What: holds an immutable `PromptLogConfig`. `log` resolves the active
/// filename (date + numeric suffix that fits under `max_bytes`), opens the
/// file in append mode, writes one line, then closes it. Every failure
/// path is a `tracing::warn!` to stderr; the caller never observes an
/// error.
/// Test: `single_event_roundtrip`, `rotation_at_size_cap`,
/// `retention_prunes_old_files`, `disabled_mode_writes_nothing`,
/// `hash_mode_hashes_trigger_prompt`.
#[derive(Clone, Debug)]
pub struct PromptLogger {
    config: PromptLogConfig,
}

impl PromptLogger {
    /// Build a logger from the configured `data_root` and process env vars.
    ///
    /// Why: keeps the call site in `prompt_context.rs` / `inbox_check.rs` to
    /// a single line and centralises the env-parsing rules.
    /// What: resolves `<data_root>` via [`trusty_common::resolve_data_dir`]
    /// using the canonical `trusty-memory` app name, then layers env overrides
    /// via [`PromptLogConfig::from_env_with_root`]. Returns a disabled logger
    /// when the data dir cannot be resolved — the caller proceeds normally.
    /// Test: covered indirectly by the integration tests.
    pub fn from_env() -> Self {
        let data_root = trusty_common::resolve_data_dir("trusty-memory")
            .unwrap_or_else(|_| std::env::temp_dir().join("trusty-memory"));
        Self::from_config(PromptLogConfig::from_env_with_root(&data_root))
    }

    /// Build a logger from an explicit config (test injection point).
    ///
    /// Why: integration / unit tests want to pin a tempdir without polluting
    /// process env. Same shape as `from_env`, different injection.
    /// Test: every unit test in this module.
    pub fn from_config(config: PromptLogConfig) -> Self {
        Self { config }
    }

    /// Active configuration (for tests / diagnostics).
    pub fn config(&self) -> &PromptLogConfig {
        &self.config
    }

    /// Append one entry to the active log file.
    ///
    /// Why: the public API surface — exactly one call per hook invocation.
    /// Best-effort by contract.
    /// What: short-circuits when `enabled = false`; otherwise computes the
    /// active filename (creating the directory and pruning stale files as
    /// needed), serialises the entry to a single JSON line, and appends it.
    /// Any failure (mkdir, open, write, serde) is downgraded to a
    /// `tracing::warn!` and discarded.
    /// Test: see module-level `tests`.
    pub fn log(&self, entry: PromptLogEntry) {
        if !self.config.enabled {
            return;
        }

        // Apply hash transform before serialising so it lands on disk.
        let entry = self.apply_privacy(entry);

        // Ensure the log directory exists.
        if let Err(e) = std::fs::create_dir_all(&self.config.dir) {
            tracing::warn!(
                "trusty-memory prompt log: could not create {}: {e}",
                self.config.dir.display()
            );
            return;
        }

        // Opportunistic retention prune — cheap (one read_dir) and only fires
        // when the day's first write reaches this point.
        self.prune_if_needed();

        // Resolve filename and append.
        let path = match self.resolve_active_path(entry.timestamp) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("trusty-memory prompt log: resolve path: {e}");
                return;
            }
        };

        let line = match serde_json::to_string(&entry) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("trusty-memory prompt log: serialise entry: {e}");
                return;
            }
        };

        match OpenOptions::new().create(true).append(true).open(&path) {
            Ok(mut f) => {
                if let Err(e) = writeln!(f, "{line}") {
                    tracing::warn!("trusty-memory prompt log: write {}: {e}", path.display());
                }
            }
            Err(e) => {
                tracing::warn!("trusty-memory prompt log: open {}: {e}", path.display());
            }
        }
    }

    /// Apply privacy transformations to the entry.
    fn apply_privacy(&self, mut entry: PromptLogEntry) -> PromptLogEntry {
        if self.config.hash_prompts {
            entry.trigger_prompt = hash_prompt(&entry.trigger_prompt);
        }
        entry
    }

    /// Resolve the path of the active log file for `timestamp`.
    ///
    /// Why: encapsulates the date prefix + numeric-suffix logic so the write
    /// path stays linear. Returns the first numeric suffix whose file is
    /// either missing or under the size cap.
    /// What: enumerates `enriched-prompts.<date>.jsonl`,
    /// `enriched-prompts.<date>.1.jsonl`, … and picks the smallest index
    /// whose file size is below `max_bytes`. Stops at a hard ceiling
    /// (`u32::MAX`) to prevent unbounded scanning if the cap is set to 0
    /// by mistake (defended further by `from_env_with_root`'s `filter`).
    /// Test: `rotation_at_size_cap`.
    fn resolve_active_path(&self, timestamp: DateTime<Utc>) -> std::io::Result<PathBuf> {
        let date_str = format!(
            "{:04}-{:02}-{:02}",
            timestamp.year(),
            timestamp.month(),
            timestamp.day()
        );
        let base = self
            .config
            .dir
            .join(format!("{FILE_PREFIX}.{date_str}.{FILE_EXT}"));
        // Index 0 is the bare `<date>.jsonl` file (no numeric suffix).
        let path_for = |i: u32| -> PathBuf {
            if i == 0 {
                base.clone()
            } else {
                self.config
                    .dir
                    .join(format!("{FILE_PREFIX}.{date_str}.{i}.{FILE_EXT}"))
            }
        };
        for i in 0u32..=u32::MAX {
            let candidate = path_for(i);
            let size = match std::fs::metadata(&candidate) {
                Ok(m) => m.len(),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
                Err(e) => return Err(e),
            };
            if size < self.config.max_bytes {
                return Ok(candidate);
            }
        }
        // Astronomically unlikely (would require writing 50 MiB × 2^32 in a
        // single day). Fall back to suffix u32::MAX so the write still lands.
        Ok(path_for(u32::MAX))
    }

    /// Prune log files older than `retention_days`.
    ///
    /// Why: keeps unbounded disk growth in check without a daemon worker. The
    /// check is cheap (one `read_dir`) so running it on every write is fine;
    /// we still gate by file presence to avoid spinning before the first
    /// write succeeds.
    /// What: parses the `<date>` component out of each
    /// `enriched-prompts.YYYY-MM-DD[.n].jsonl` filename and removes files
    /// older than `today - retention_days`. Unparseable filenames are left
    /// alone. Errors are logged at `warn!` and ignored.
    /// Test: `retention_prunes_old_files`.
    fn prune_if_needed(&self) {
        let today = Utc::now().date_naive();
        let cutoff =
            match today.checked_sub_days(chrono::Days::new(self.config.retention_days as u64)) {
                Some(d) => d,
                None => return,
            };
        let dir = match std::fs::read_dir(&self.config.dir) {
            Ok(d) => d,
            Err(_) => return,
        };
        for entry in dir.flatten() {
            let name = entry.file_name();
            let name = match name.to_str() {
                Some(s) => s,
                None => continue,
            };
            let date = match parse_log_filename_date(name) {
                Some(d) => d,
                None => continue,
            };
            if date < cutoff {
                if let Err(e) = std::fs::remove_file(entry.path()) {
                    tracing::warn!(
                        "trusty-memory prompt log: prune {}: {e}",
                        entry.path().display()
                    );
                }
            }
        }
    }
}

/// Parse the date out of `enriched-prompts.YYYY-MM-DD[.n].jsonl`.
///
/// Why: retention pruning needs to identify the date stamp without parsing
/// every JSONL line. Returning `None` for unrelated files keeps the prune
/// idempotent — we never touch files we don't recognise.
/// What: strips the `enriched-prompts.` prefix and `.jsonl` (or `.N.jsonl`)
/// suffix; parses what's left as `NaiveDate`. Returns `None` on any
/// shape mismatch.
/// Test: `parse_filename_date_parses_canonical_and_rotated`.
fn parse_log_filename_date(name: &str) -> Option<NaiveDate> {
    let prefix = format!("{FILE_PREFIX}.");
    let suffix = format!(".{FILE_EXT}");
    let inner = name.strip_prefix(&prefix)?.strip_suffix(&suffix)?;
    // `inner` is either `YYYY-MM-DD` or `YYYY-MM-DD.N`.
    let date_part = match inner.find('.') {
        Some(i) => &inner[..i],
        None => inner,
    };
    NaiveDate::parse_from_str(date_part, "%Y-%m-%d").ok()
}

/// SHA-256 the supplied prompt and prefix with `sha256:`.
///
/// Why: the privacy-preserving alternative to logging raw user input.
/// What: returns `sha256:<lowercase hex>` so consumers can spot the
/// transformed field at a glance.
/// Test: `hash_mode_hashes_trigger_prompt`.
fn hash_prompt(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    let digest = hasher.finalize();
    format!("sha256:{digest:x}")
}

/// True when the value looks like an explicit off switch.
fn is_off(v: &str) -> bool {
    matches!(
        v.trim().to_ascii_lowercase().as_str(),
        "0" | "off" | "false" | "no" | "disabled"
    )
}

/// True when the value looks like an explicit on switch.
fn is_on(v: &str) -> bool {
    matches!(
        v.trim().to_ascii_lowercase().as_str(),
        "1" | "on" | "true" | "yes" | "enabled"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Helper: build a logger pointed at a tempdir's `logs/` subdir.
    fn logger_in(
        dir: &Path,
        hash_prompts: bool,
        max_bytes: u64,
        retention_days: u32,
    ) -> PromptLogger {
        PromptLogger::from_config(PromptLogConfig {
            enabled: true,
            dir: dir.join("logs"),
            max_bytes,
            retention_days,
            hash_prompts,
        })
    }

    fn read_jsonl_lines(path: &Path) -> Vec<String> {
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .map(|l| l.to_string())
            .collect()
    }

    fn list_log_files(dir: &Path) -> Vec<PathBuf> {
        let logs_dir = dir.join("logs");
        let mut out: Vec<PathBuf> = std::fs::read_dir(&logs_dir)
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.path())
                    .filter(|p| {
                        p.file_name()
                            .and_then(|n| n.to_str())
                            .is_some_and(|n| n.starts_with(FILE_PREFIX))
                    })
                    .collect()
            })
            .unwrap_or_default();
        out.sort();
        out
    }

    /// Why: every other test in the module depends on the basic round-trip
    /// shape. This pins it.
    /// What: write one entry through `log`, find the resulting file, parse
    /// the single line, and assert all fields survive intact.
    /// Test: itself.
    #[test]
    fn single_event_roundtrip() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let logger = logger_in(tmp.path(), false, DEFAULT_MAX_BYTES, 30);

        let entry = PromptLogEntry::new(
            "UserPromptSubmit",
            "prompt-context-facts",
            "test-palace",
            "what tools should I use?",
            "## Context\n- alias: tm -> trusty-memory\n",
        )
        .with_duration_ms(12)
        .with_palace_facts_count(7);

        logger.log(entry.clone());

        let files = list_log_files(tmp.path());
        assert_eq!(
            files.len(),
            1,
            "expected exactly one log file, got {files:?}"
        );
        let lines = read_jsonl_lines(&files[0]);
        assert_eq!(lines.len(), 1, "expected one JSONL line, got {lines:?}");
        let parsed: PromptLogEntry = serde_json::from_str(&lines[0]).expect("parse JSONL entry");

        assert_eq!(parsed.hook_type, "UserPromptSubmit");
        assert_eq!(parsed.injection_kind, "prompt-context-facts");
        assert_eq!(parsed.palace, "test-palace");
        assert_eq!(parsed.trigger_prompt, "what tools should I use?");
        assert_eq!(parsed.injection, entry.injection);
        assert_eq!(parsed.injection_length, entry.injection.len());
        assert_eq!(parsed.palace_facts_count, Some(7));
        assert_eq!(parsed.unread_messages_count, None);
        assert_eq!(parsed.duration_ms, 12);
    }

    /// Why: size-based rotation is the harder of the two rotation rules to
    /// get right; date rotation only fires once a day. We pin a tiny cap and
    /// write enough entries to force at least one roll.
    /// What: max_bytes = 200; write 5 entries with ~120-byte injections; assert
    /// at least two log files exist after the run.
    /// Test: itself.
    #[test]
    fn rotation_at_size_cap() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let logger = logger_in(tmp.path(), false, 200, 30);

        for i in 0..5 {
            let entry = PromptLogEntry::new(
                "UserPromptSubmit",
                "prompt-context-facts",
                "test-palace",
                format!("prompt #{i} with some padding to push us over the cap"),
                format!("injection #{i} with some padding to push us over the cap"),
            )
            .with_duration_ms(i as u64);
            logger.log(entry);
        }

        let files = list_log_files(tmp.path());
        assert!(
            files.len() >= 2,
            "expected rotation to produce at least two files, got {files:?}"
        );
    }

    /// Why: stale files must be pruned so disk usage stays bounded. Forge a
    /// file with a date older than the window and assert it disappears on
    /// the next write.
    /// What: retention=2 days; pre-create `enriched-prompts.<old>.jsonl`
    /// dated 90 days ago; write a fresh entry; assert the stale file is
    /// gone and the new file exists.
    /// Test: itself.
    #[test]
    fn retention_prunes_old_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let logs_dir = tmp.path().join("logs");
        std::fs::create_dir_all(&logs_dir).unwrap();

        // Forge a stale log file dated 90 days ago.
        let stale_date = Utc::now()
            .date_naive()
            .checked_sub_days(chrono::Days::new(90))
            .expect("stale date");
        let stale_name = format!(
            "{FILE_PREFIX}.{:04}-{:02}-{:02}.{FILE_EXT}",
            stale_date.year(),
            stale_date.month(),
            stale_date.day()
        );
        let stale_path = logs_dir.join(&stale_name);
        std::fs::write(&stale_path, "{\"stale\": true}\n").unwrap();

        // Also forge an unrelated file that must NOT be pruned.
        let unrelated = logs_dir.join("not-our-log.txt");
        std::fs::write(&unrelated, "ignore me").unwrap();

        let logger = logger_in(tmp.path(), false, DEFAULT_MAX_BYTES, 2);
        logger.log(PromptLogEntry::new(
            "UserPromptSubmit",
            "prompt-context-facts",
            "test-palace",
            "trigger",
            "injection",
        ));

        assert!(
            !stale_path.exists(),
            "stale log file at {} should have been pruned",
            stale_path.display()
        );
        assert!(
            unrelated.exists(),
            "unrelated file at {} must not be touched",
            unrelated.display()
        );
        let files = list_log_files(tmp.path());
        // A fresh entry must have produced *some* current-day file.
        let today = Utc::now().date_naive();
        let expected_today = format!(
            "{FILE_PREFIX}.{:04}-{:02}-{:02}.{FILE_EXT}",
            today.year(),
            today.month(),
            today.day()
        );
        assert!(
            files.iter().any(|p| p
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n == expected_today)),
            "expected today's log file `{expected_today}` to exist, got {files:?}"
        );
    }

    /// Why: the opt-out switch is the most important privacy guarantee.
    /// What: build a disabled logger, write one entry, assert no files exist
    /// under the configured directory.
    /// Test: itself.
    #[test]
    fn disabled_mode_writes_nothing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let logger = PromptLogger::from_config(PromptLogConfig {
            enabled: false,
            dir: tmp.path().join("logs"),
            max_bytes: DEFAULT_MAX_BYTES,
            retention_days: 30,
            hash_prompts: false,
        });
        logger.log(PromptLogEntry::new(
            "UserPromptSubmit",
            "prompt-context-facts",
            "test-palace",
            "trigger",
            "injection",
        ));

        // The logs directory should not be created.
        assert!(
            !tmp.path().join("logs").exists(),
            "disabled logger must not create the log directory"
        );
    }

    /// Why: the hash-prompts mode is the second privacy guarantee — raw user
    /// input must never land on disk.
    /// What: enable `hash_prompts`, write an entry with a known prompt,
    /// parse the resulting JSON, assert `trigger_prompt` starts with
    /// `sha256:` and matches a known digest. Also assert the raw prompt
    /// text never appears in the file.
    /// Test: itself.
    #[test]
    fn hash_mode_hashes_trigger_prompt() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let logger = logger_in(tmp.path(), true, DEFAULT_MAX_BYTES, 30);

        let raw_prompt = "secret user prompt that must not land on disk";
        logger.log(PromptLogEntry::new(
            "UserPromptSubmit",
            "prompt-context-facts",
            "test-palace",
            raw_prompt,
            "injection body",
        ));

        let files = list_log_files(tmp.path());
        assert_eq!(files.len(), 1);
        let content = std::fs::read_to_string(&files[0]).unwrap();
        assert!(
            !content.contains(raw_prompt),
            "raw prompt must not appear in the log file; got {content}"
        );
        let parsed: PromptLogEntry = serde_json::from_str(content.trim()).expect("parse JSONL");
        assert!(
            parsed.trigger_prompt.starts_with("sha256:"),
            "trigger_prompt should be hashed, got {}",
            parsed.trigger_prompt
        );
        // Cross-check the digest.
        assert_eq!(parsed.trigger_prompt, hash_prompt(raw_prompt));
    }

    /// Why: the env-driven config path is the production code path. Test it
    /// directly so the rules cannot drift silently.
    /// What: with no env set, defaults are picked up; with the off switch,
    /// `enabled = false`; with explicit overrides, custom values appear.
    /// Test: itself.
    #[tokio::test]
    async fn config_from_env_defaults() {
        // Serialise with the commands::env_test_lock so this test cannot race
        // the env-touching integration tests in `commands::prompt_context`
        // / `commands::inbox_check`.
        let _guard = crate::commands::env_test_lock().lock().await;
        let tmp = tempfile::tempdir().expect("tempdir");
        // Snapshot and clear so the test doesn't observe contamination from
        // other tests in the same process.
        let prev_enabled = std::env::var(ENV_ENABLED).ok();
        let prev_dir = std::env::var(ENV_DIR).ok();
        let prev_max = std::env::var(ENV_MAX_BYTES).ok();
        let prev_ret = std::env::var(ENV_RETENTION_DAYS).ok();
        let prev_hash = std::env::var(ENV_HASH_PROMPTS).ok();
        // SAFETY: env mutation. Restored at end of test.
        unsafe {
            std::env::remove_var(ENV_ENABLED);
            std::env::remove_var(ENV_DIR);
            std::env::remove_var(ENV_MAX_BYTES);
            std::env::remove_var(ENV_RETENTION_DAYS);
            std::env::remove_var(ENV_HASH_PROMPTS);
        }
        let cfg = PromptLogConfig::from_env_with_root(tmp.path());
        assert!(cfg.enabled);
        assert_eq!(cfg.dir, tmp.path().join("logs"));
        assert_eq!(cfg.max_bytes, DEFAULT_MAX_BYTES);
        assert_eq!(cfg.retention_days, DEFAULT_RETENTION_DAYS);
        assert!(!cfg.hash_prompts);
        // Restore.
        unsafe {
            for (k, v) in [
                (ENV_ENABLED, prev_enabled),
                (ENV_DIR, prev_dir),
                (ENV_MAX_BYTES, prev_max),
                (ENV_RETENTION_DAYS, prev_ret),
                (ENV_HASH_PROMPTS, prev_hash),
            ] {
                if let Some(val) = v {
                    std::env::set_var(k, val);
                } else {
                    std::env::remove_var(k);
                }
            }
        }
    }

    /// Why: every value of the off-switch must produce a disabled logger.
    #[test]
    fn is_off_matches_documented_values() {
        for v in ["0", "off", "OFF", "Off", "false", "False", "no", "disabled"] {
            assert!(is_off(v), "{v} should be parsed as off");
        }
        for v in ["1", "on", "true", "yes", "yeah", ""] {
            assert!(!is_off(v), "{v} should NOT be parsed as off");
        }
    }

    /// Why: hash-mode toggle has its own truthiness set.
    #[test]
    fn is_on_matches_documented_values() {
        for v in ["1", "on", "ON", "true", "True", "yes", "enabled"] {
            assert!(is_on(v), "{v} should be parsed as on");
        }
        for v in ["0", "off", "false", "no", ""] {
            assert!(!is_on(v), "{v} should NOT be parsed as on");
        }
    }

    /// Why: the filename parser is the linchpin of retention. Pin its
    /// recognised shapes so retention can't accidentally start deleting
    /// random files.
    #[test]
    fn parse_filename_date_parses_canonical_and_rotated() {
        let canonical = "enriched-prompts.2026-05-25.jsonl";
        let rotated = "enriched-prompts.2026-05-25.3.jsonl";
        let canonical_date = parse_log_filename_date(canonical).expect("canonical parses");
        let rotated_date = parse_log_filename_date(rotated).expect("rotated parses");
        assert_eq!(canonical_date, rotated_date);
        assert_eq!(
            canonical_date,
            NaiveDate::from_ymd_opt(2026, 5, 25).unwrap()
        );

        for bad in [
            "not-our-log.txt",
            "enriched-prompts..jsonl",
            "enriched-prompts.bogus.jsonl",
            "enriched-prompts.2026-13-99.jsonl",
            "enriched-prompts.2026-05-25.txt",
            "other-prefix.2026-05-25.jsonl",
        ] {
            assert!(
                parse_log_filename_date(bad).is_none(),
                "should not parse: {bad}"
            );
        }
    }
}
