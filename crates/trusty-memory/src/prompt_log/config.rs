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

/// What shaping did to the recall query before it was embedded (#4972).
///
/// Why: the query used to go to the embedder whole and come back cut at the
/// 512-token window with "no warning, no metric, and no signal to the caller"
/// — the defect as filed. This struct is the metric. It rides the enriched-
/// prompt log line, which is the same corpus the 52%-over-window rate was
/// measured from, so the rate after the fix is two `jq` filters away:
/// `select(.recall_query.units_dropped > 0)` for queries this module reduced,
/// and `select(.recall_query.sent_tokens_max > .recall_query.budget_tokens)` for
/// sends whose fit inside the window could not be proven.
/// What: token estimates before and after shaping, the ceiling on what was
/// sent, the budget in force, and what was removed. Absent from the JSON when
/// no palace was resolved and no recall was attempted.
/// Test: `single_event_roundtrip` covers serialisation;
/// `prompt_context::tests::over_window_query_is_reduced_to_whole_units` covers
/// the values.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecallQueryShape {
    /// Estimated tokens in the raw prompt, before any shaping.
    pub original_tokens: usize,
    /// Estimated tokens actually sent to `/recall`.
    pub sent_tokens: usize,
    /// Upper bound on the true tokens sent (#4972).
    ///
    /// Why: `sent_tokens` charges ASCII-letter runs by a calibrated divisor,
    /// which no divisor above 1 token per character can make a bound. Without
    /// this field a shape reporting `sent_tokens: 490, units_dropped: 0` asserts
    /// a clean pass on a query the embedder may have cut — the metric reading
    /// healthy while the loss happens.
    /// What: `prompt_context::query::max_tokens` of the sent text.
    /// `#[serde(default)]` so log lines written before this field parse back.
    #[serde(default)]
    pub sent_tokens_max: usize,
    /// Token budget in force for this firing.
    pub budget_tokens: usize,
    /// Whether a task-notification envelope was reduced to its payload.
    pub envelope_stripped: bool,
    /// Whole units (lines, or words) dropped to fit the budget. On the
    /// last-resort character path the unit is the character.
    pub units_dropped: usize,
}

impl RecallQueryShape {
    /// True when shaping changed the query the embedder saw.
    ///
    /// Why: the pass-through case is the common one and must stay quiet — a
    /// warn on every firing is a warn nobody reads.
    /// What: `envelope_stripped || units_dropped > 0`.
    /// Test: `prompt_context::tests::short_query_passes_through_untouched`.
    pub fn reshaped(&self) -> bool {
        self.envelope_stripped || self.units_dropped > 0
    }

    /// True when the sent query is *not* provably inside the embedder window.
    ///
    /// Why (#4972, round-3 review): `sent_tokens <= budget_tokens` is an
    /// estimate clearing a budget, not a proof, and treating it as one is what
    /// let a reshaped query still overrun the window while the log reported the
    /// reduction as a success. This is the honest complement — false means the
    /// send fits, full stop; true means it may have been cut and the shape
    /// declines to claim otherwise.
    /// What: `sent_tokens_max > budget_tokens`.
    /// Test: `prompt_context::tests::shape_flags_a_send_it_cannot_prove_fits`.
    pub fn may_exceed_window(&self) -> bool {
        self.sent_tokens_max > self.budget_tokens
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
    /// How the recall query was shaped before embedding (#4972). `None` when no
    /// palace resolved, so no recall was attempted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recall_query: Option<RecallQueryShape>,
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
            recall_query: None,
            duration_ms: 0,
        }
    }

    /// Builder: attach how the recall query was shaped (prompt-context only).
    ///
    /// Why (#4972): the shaping is only observable if it reaches the log.
    /// What: sets `recall_query`; `None` leaves the field off the JSON line.
    /// Test: `prompt_context::tests::over_window_query_is_reduced_to_whole_units`.
    #[must_use]
    pub fn with_recall_query(mut self, shape: Option<RecallQueryShape>) -> Self {
        self.recall_query = shape;
        self
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
