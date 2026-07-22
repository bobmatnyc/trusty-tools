//! Daemon activity events and their persistence fallback.
//!
//! Why: the SSE dashboard and the persistent activity log both need a single
//! typed vocabulary for "something happened" (palace created, drawer added,
//! dream completed, hook fired). Keeping the event enum, its hook/injection
//! labels, and the best-effort log-open fallback together — separate from the
//! `AppState` plumbing in `lib.rs` — keeps each file focused and under the
//! SLOC cap.
//! What: exports `DaemonEvent`, `HookType`, `InjectionKind`, and the
//! crate-internal `open_activity_log_with_fallback` helper.
//! Test: `lib_tests` covers `type_str`/`palace_id`/`source` extraction, the
//! serde round-trips, and the discard fallback branch.

use crate::{ActivityLog, ActivitySource};
use std::path::Path;
use std::sync::Arc;

/// Hook type — labels the Claude Code hook that triggered a submission.
///
/// Why: every hook firing produces an activity-feed entry tagged with the
/// originating hook so operators can tell whether activity came from a user
/// prompt (`UserPromptSubmit`), a new session (`SessionStart`), or a future
/// hook variant. Threading this through `DaemonEvent::HookFired` lets the
/// dashboard badge each row with the hook label.
/// What: serde-serialised in PascalCase so the wire format matches Claude
/// Code's own hook-name strings exactly (e.g. `"UserPromptSubmit"`).
/// Test: `hook_type_serde_round_trips`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum HookType {
    /// Claude Code's `UserPromptSubmit` hook — fires on every user prompt.
    UserPromptSubmit,
    /// Claude Code's `SessionStart` hook — fires once at session open.
    SessionStart,
}

impl HookType {
    /// Stable string label used for the wire format.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::UserPromptSubmit => "UserPromptSubmit",
            Self::SessionStart => "SessionStart",
        }
    }
}

/// Injection kind — labels what the hook actually injected (or attempted).
///
/// Why: distinct from `HookType` because one hook could in principle render
/// more than one kind of injection (e.g. SessionStart can deliver both an
/// inbox check and bootstrap context). Tagging the rendered kind explicitly
/// keeps the activity log searchable when that fan-out lands.
/// What: serde-serialised as kebab-case so it matches the labels already
/// used in the JSONL prompt log (`prompt-context-facts`,
/// `inbox-check-messages`).
/// Test: `injection_kind_serde_round_trips`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InjectionKind {
    /// `prompt-context` hook rendered the prompt-facts block.
    PromptContext,
    /// `inbox-check` hook delivered unread messages.
    InboxCheck,
}

impl InjectionKind {
    /// Stable string label used for the wire format.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::PromptContext => "prompt-context",
            Self::InboxCheck => "inbox-check",
        }
    }
}

/// Live daemon events broadcast to connected SSE subscribers.
///
/// Why: The dashboard needs push-driven updates so palace creation, drawer
/// add/delete, dream cycles, and aggregate status changes are visible without
/// polling. A single broadcast channel fans out to every connected browser.
/// What: Tagged enum serialized as `{"type": "...", ...fields}` over SSE.
/// Test: `web::tests::sse_stream_emits_events` subscribes, triggers a
/// mutation, and asserts the frame arrives.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DaemonEvent {
    PalaceCreated {
        id: String,
        name: String,
        /// Originating subsystem (HTTP, MCP, Hook). Why (issue #96): the
        /// UI badges each row with its source so operators can tell at a
        /// glance whether a write came from the dashboard form, an MCP
        /// tool call, or a hook-driven path. The wire-format key is
        /// `source` (lower-case strings via serde rename_all on
        /// `ActivitySource`).
        source: ActivitySource,
    },
    DrawerAdded {
        palace_id: String,
        /// Friendly palace name (Palace.name) at write time. Why: lets SSE
        /// consumers (the dashboard activity feed) render the human-readable
        /// label without a separate id→name lookup. Empty string if the
        /// emitter could not resolve the name.
        #[serde(default)]
        palace_name: String,
        drawer_count: usize,
        /// Wall-clock timestamp when the drawer was added. Why: SSE
        /// receivers want to render "just now / 2m ago" relative to the
        /// daemon's clock, not the time the SSE frame happens to arrive.
        timestamp: chrono::DateTime<chrono::Utc>,
        /// Short preview of the drawer's content (whitespace-collapsed,
        /// truncated to ~80 chars with an ellipsis when cut). Why: the TUI
        /// activity feed and dashboard ticker want to show *what* was
        /// stored, not just the running drawer count. Empty when the
        /// emitter could not resolve the content (legacy clients tolerate
        /// the missing field via `#[serde(default)]`).
        #[serde(default)]
        content_preview: String,
        /// Originating subsystem (issue #96).
        source: ActivitySource,
    },
    DrawerDeleted {
        palace_id: String,
        drawer_count: usize,
        /// Originating subsystem (issue #96).
        source: ActivitySource,
    },
    DreamCompleted {
        palace_id: Option<String>,
        merged: usize,
        pruned: usize,
        compacted: usize,
        closets_updated: usize,
        duration_ms: u64,
        /// Originating subsystem (issue #96).
        source: ActivitySource,
    },
    StatusChanged {
        total_drawers: usize,
        total_vectors: usize,
        total_kg_triples: usize,
    },
    /// A Claude Code hook completed and rendered (or attempted to render) an
    /// injection block.
    ///
    /// Why: pre-#XXX the activity feed only fired on drawer / palace / dream
    /// writes, which meant a normal Claude Code session — whose only daemon
    /// traffic is hook invocations — left the feed empty. Surfacing every
    /// hook firing answers the user complaint "no activity in the TUI" and
    /// gives operators a way to see how often each project palace is
    /// actually picking up prompt-context / inbox-check work.
    /// What: carries the resolved palace (or `None` if cwd resolution
    /// failed), the [`HookType`] label, the [`InjectionKind`] label, the
    /// rendered injection byte length, a short excerpt of the triggering
    /// prompt (capped at ~80 chars; the full content stays in the JSONL
    /// prompt log only), the timestamp, the hook's wall-clock duration,
    /// and the [`ActivitySource`] tag (always `Hook` for this variant).
    /// Backwards-compatible: SSE clients that do not recognise the
    /// `hook_fired` `type` tag can safely ignore the frame.
    HookFired {
        /// Resolved palace id (slug) — `None` if cwd resolution failed.
        #[serde(default)]
        palace_id: Option<String>,
        /// Friendly palace name at hook time — `None` if the registry
        /// could not be consulted (HTTP path uses `palace_id` here when
        /// no separate name is known).
        #[serde(default)]
        palace_name: Option<String>,
        hook_type: HookType,
        injection_kind: InjectionKind,
        /// Rendered injection size in bytes (`0` when no injection was
        /// emitted, e.g. SessionStart with an empty inbox).
        injection_length: u64,
        /// Short excerpt of the triggering prompt for the activity feed
        /// display. Capped at ~80 chars with a trailing `…` when cut.
        /// Why: the activity feed renders this directly; full prompt
        /// content (which may be sensitive) stays in the JSONL log.
        #[serde(default)]
        trigger_prompt_excerpt: String,
        timestamp: chrono::DateTime<chrono::Utc>,
        /// Hook wall-clock duration in milliseconds.
        duration_ms: u64,
        /// Always `ActivitySource::Hook` for this variant; encoded explicitly
        /// so the same dispatch path (`emit`) can persist + broadcast it.
        source: ActivitySource,
    },
}

impl DaemonEvent {
    /// Short discriminant label matching the SSE `type` field.
    ///
    /// Why: the persisted activity log stores `event_type` as a string so
    /// the UI can render the row without re-parsing the payload. Sharing
    /// the same labels the SSE serializer uses keeps the wire and the
    /// stored history consistent.
    /// What: returns one of `palace_created`, `drawer_added`,
    /// `drawer_deleted`, `dream_completed`, `status_changed`.
    /// Test: `daemon_event_type_str_matches_sse_tag` in the lib tests.
    pub fn type_str(&self) -> &'static str {
        match self {
            Self::PalaceCreated { .. } => "palace_created",
            Self::DrawerAdded { .. } => "drawer_added",
            Self::DrawerDeleted { .. } => "drawer_deleted",
            Self::DreamCompleted { .. } => "dream_completed",
            Self::StatusChanged { .. } => "status_changed",
            Self::HookFired { .. } => "hook_fired",
        }
    }

    /// `palace_id` if the event is scoped to a single palace.
    ///
    /// Why: the activity log indexes entries by palace id so the UI can
    /// filter by palace; daemon-wide events (`status_changed`,
    /// dream-across-all-palaces) return `None`.
    /// What: returns a borrowed string when the variant carries a palace
    /// id, otherwise `None`.
    /// Test: `daemon_event_palace_id_extraction`.
    pub fn palace_id(&self) -> Option<&str> {
        match self {
            Self::PalaceCreated { id, .. } => Some(id),
            Self::DrawerAdded { palace_id, .. } | Self::DrawerDeleted { palace_id, .. } => {
                Some(palace_id)
            }
            Self::DreamCompleted { palace_id, .. } => palace_id.as_deref(),
            Self::HookFired { palace_id, .. } => palace_id.as_deref(),
            Self::StatusChanged { .. } => None,
        }
    }

    /// Originating subsystem if the event carries one.
    ///
    /// Why: only mutation events carry a `source`; the aggregate
    /// `StatusChanged` is recomputed by the daemon and has no caller, so
    /// it returns `None`.
    /// What: returns the variant's `source` field where present.
    /// Test: `daemon_event_source_extraction`.
    pub fn source(&self) -> Option<ActivitySource> {
        match self {
            Self::PalaceCreated { source, .. }
            | Self::DrawerAdded { source, .. }
            | Self::DrawerDeleted { source, .. }
            | Self::DreamCompleted { source, .. }
            | Self::HookFired { source, .. } => Some(*source),
            Self::StatusChanged { .. } => None,
        }
    }
}

/// Open the activity log under `data_root`, falling back to a per-process
/// tempdir and finally to a no-op `Discard` variant when no writable
/// directory is available.
///
/// Why (issues #96, #225): the activity log is a best-effort feature — if
/// the data root is on a read-only mount, missing, or locked by another
/// process, the daemon should still come up and serve every other endpoint.
/// The first fallback is a `std::env::temp_dir()`-anchored subdirectory
/// keyed by the daemon's process id. Issue #225: a previous version called
/// `expect()` on the tempdir fallback, which crashed the daemon on hosts
/// where neither `data_root` nor `std::env::temp_dir()` is writable
/// (read-only containers, locked-down sandboxes). The contract is
/// "best-effort", so the final fallback is now `ActivityLog::discard()` —
/// a no-op variant that drops every append and returns empty reads. The
/// dashboard's activity feed simply shows up empty in that degraded state.
/// What: tries `ActivityLog::open(data_root)`; on error logs a warning and
/// retries against `<temp>/trusty-memory-activity-<pid>/`. If both fail,
/// emits a final warning and returns `ActivityLog::discard()`.
/// Test: `open_activity_log_with_fallback_returns_discard_when_unwritable`
/// covers the discard branch; existing `AppState` construction tests cover
/// the happy and tempdir-fallback paths.
pub(crate) fn open_activity_log_with_fallback(data_root: &Path) -> Arc<ActivityLog> {
    open_activity_log_with_fallback_in(data_root, &std::env::temp_dir())
}

/// Same as [`open_activity_log_with_fallback`], but the tempdir-fallback
/// ROOT is an explicit parameter rather than always `std::env::temp_dir()`
/// (issue #3434).
///
/// Why: the discard-path test needs to force BOTH the primary data root and
/// the tempdir fallback to be unwritable. The previous version of that test
/// did this by mutating the process-global `TMPDIR` env var for the
/// duration of the test — but `cargo test` runs every test in this crate's
/// lib binary as threads of ONE process, so any OTHER test that calls
/// `tempfile::tempdir()` (which reads `$TMPDIR`) while the mutation was live
/// would itself fail with `PermissionDenied`, for a reason entirely
/// unrelated to its own code. Splitting the fallback root out into a
/// parameter removes the shared mutable global from this path entirely —
/// mirroring the same "thread it through instead of mutating the env var"
/// fix already applied to `trusty-code`'s `catchup::pm_catchup_context`
/// (#3003) and `session::memory_sink::TurnMemorySink` for the identical
/// class of bug — so the test needs no lock, no restore-on-panic guard, and
/// cannot leak state into any concurrently-running test.
/// What: tries `ActivityLog::open(data_root)`; on error, retries against
/// `<fallback_root>/trusty-memory-activity-<pid>/`; if that also fails,
/// returns `ActivityLog::discard()`. Identical behaviour to
/// [`open_activity_log_with_fallback`], which is now a thin wrapper passing
/// `std::env::temp_dir()` as `fallback_root`.
/// Test: `open_activity_log_with_fallback_returns_discard_when_unwritable`.
pub(crate) fn open_activity_log_with_fallback_in(
    data_root: &Path,
    fallback_root: &Path,
) -> Arc<ActivityLog> {
    match ActivityLog::open(data_root) {
        Ok(log) => Arc::new(log),
        Err(primary_err) => {
            tracing::warn!(
                "could not open activity log at {}: {primary_err:#}; falling back to per-process tempdir",
                data_root.display()
            );
            let fallback =
                fallback_root.join(format!("trusty-memory-activity-{}", std::process::id()));
            match ActivityLog::open(&fallback) {
                Ok(log) => Arc::new(log),
                Err(fallback_err) => {
                    tracing::warn!(
                        "activity log tempdir fallback at {} also failed: {fallback_err:#}; \
                         activity feed disabled for this process (no-op log)",
                        fallback.display()
                    );
                    Arc::new(ActivityLog::discard())
                }
            }
        }
    }
}
