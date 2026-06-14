//! Configuration and types for the enriched-prompt logger.
//!
//! Why: Separating the configuration + data types from the writer logic keeps
//! each file under the 500-SLOC cap and allows tests to construct configs
//! directly without importing the writer.
//! What: `PromptLogConfig`, `PromptLogEntry`, environment variable constants,
//! and the default-value constants.
//! Test: `config_from_env_defaults`, `config_from_env_disabled`, and the
//! round-trip / format tests in `writer`.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

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

/// Configuration for [`crate::prompt_log::PromptLogger`].
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

/// True when the value looks like an explicit off switch.
pub(super) fn is_off(v: &str) -> bool {
    matches!(
        v.trim().to_ascii_lowercase().as_str(),
        "0" | "off" | "false" | "no" | "disabled"
    )
}

/// True when the value looks like an explicit on switch.
pub(super) fn is_on(v: &str) -> bool {
    matches!(
        v.trim().to_ascii_lowercase().as_str(),
        "1" | "on" | "true" | "yes" | "enabled"
    )
}
