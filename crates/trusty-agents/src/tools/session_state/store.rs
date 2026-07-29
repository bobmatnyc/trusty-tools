//! Read-only reader for the managed-session store (#4171, epic #4167).
//!
//! Why: `session_state_list` / `session_state_status` need the same session
//! facts the orchestration harness persists, but `trusty-agents` deliberately
//! does NOT depend on the `trusty-mpm` crate — see the rationale on
//! `mcp::config::tests::render_tests::trusty_mpm_service_tool_names_match_expected_curated_list`,
//! which declined the dependency because it would pull a full
//! daemon/tui/telegram/slack graph into every `cargo test -p trusty-agents`.
//! So this module reads the on-disk JSON directly through its OWN minimal,
//! maximally-permissive view types. That keeps the read a pure file open (no
//! daemon, no subprocess, no socket — read-only by construction) and means a
//! schema addition on the writer's side can never break the reader.
//! What: [`SessionView`] (the subset of fields these tools surface, every one
//! `#[serde(default)]` so an older or newer record still deserializes) and
//! [`load_sessions`], which reads `{"sessions": {"<id>": {…}}}` and returns
//! the records sorted most-recently-active first. [`default_store_path`]
//! resolves `~/.trusty-mpm/session-manager/sessions.json`.
//! Test: `load_sessions_reads_records_and_sorts_by_activity`,
//! `load_sessions_tolerates_unknown_and_missing_fields`,
//! `load_sessions_absent_store_is_empty_not_an_error`,
//! `load_sessions_malformed_json_is_an_error`.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The subset of a managed-session record these read-only tools surface.
///
/// Why: Naming only what is displayed keeps the reader decoupled from the
/// writer's evolving record shape, and keeps the tool output free of fields
/// (workspace-ownership flags, provisioning internals) that answer no
/// orchestration question. Every field is `#[serde(default)]` so neither a
/// field added by a newer writer nor one absent in an older record can turn a
/// legible session list into a parse error.
/// What: identity (`id`, `tmux_name`), placement (`cwd`, `workspace_path`,
/// `branch`, `source_id`), intent (`task`), lifecycle (`state`,
/// `created_at`, `last_activity_at`) and any surfaced pending decision.
/// Test: `load_sessions_tolerates_unknown_and_missing_fields`.
#[derive(Debug, Clone, Default, Deserialize)]
pub(super) struct SessionView {
    /// Managed session id (stringified UUID).
    #[serde(default)]
    pub id: String,
    /// tmux session name, e.g. `tm-quiet-falcon`.
    #[serde(default)]
    pub tmux_name: String,
    /// Working directory the session was started in.
    #[serde(default)]
    pub cwd: String,
    /// Human-readable task description supplied at creation.
    #[serde(default)]
    pub task: String,
    /// Lifecycle state as persisted (e.g. `running`, `paused`, `stopped`).
    #[serde(default)]
    pub state: String,
    /// RFC3339 creation timestamp.
    #[serde(default)]
    pub created_at: String,
    /// RFC3339 last-activity timestamp, when the writer recorded one.
    #[serde(default)]
    pub last_activity_at: Option<String>,
    /// Isolated workspace path, when the session has one.
    #[serde(default)]
    pub workspace_path: Option<String>,
    /// Git branch / ref the workspace is checked out at.
    #[serde(default)]
    pub branch: Option<String>,
    /// Project identity (e.g. `owner/repo`) when the session carries one.
    #[serde(default)]
    pub source_id: Option<String>,
    /// A decision the harness is waiting on, when one is pending.
    #[serde(default)]
    pub pending_decision: Option<String>,
}

impl SessionView {
    /// The sort key used to order the session list, newest activity first.
    ///
    /// Why: an orchestrator asking "what is in flight?" wants the sessions
    /// that moved most recently at the top. Falling back to `created_at` (not
    /// to the empty string) keeps a never-active session ordered by age
    /// instead of collapsing every one of them into an arbitrary clump.
    /// What: `last_activity_at` when present, else `created_at`. RFC3339
    /// timestamps sort correctly as plain strings, so no date parsing (and no
    /// parse-failure branch) is needed.
    /// Test: `load_sessions_reads_records_and_sorts_by_activity`.
    pub fn activity_key(&self) -> &str {
        self.last_activity_at
            .as_deref()
            .unwrap_or(self.created_at.as_str())
    }

    /// Whether `needle` identifies this session.
    ///
    /// Why: `session_state_status` is called by a model that has just read a
    /// `session_state_list` line, so it may hold either the id or the tmux
    /// name — and often an abbreviated id. Accepting both, plus an id prefix,
    /// avoids a pointless "not found" round trip.
    /// What: case-insensitive equality against `id` or `tmux_name`, or a
    /// case-insensitive `id` prefix of at least 6 characters (short enough to
    /// be convenient, long enough that a match is not accidental). The prefix
    /// slice goes through `str::get` rather than `[..n]` so a store record
    /// carrying a non-ASCII id can never panic the reader on a char boundary.
    /// Test: `status_matches_by_id_tmux_name_and_id_prefix`,
    /// `status_rejects_too_short_a_prefix`.
    pub fn matches(&self, needle: &str) -> bool {
        const MIN_PREFIX: usize = 6;
        if self.id.eq_ignore_ascii_case(needle) || self.tmux_name.eq_ignore_ascii_case(needle) {
            return true;
        }
        needle.len() >= MIN_PREFIX
            && self
                .id
                .get(..needle.len())
                .is_some_and(|p| p.eq_ignore_ascii_case(needle))
    }
}

/// The on-disk envelope: a map from session id to record.
#[derive(Debug, Default, Deserialize)]
struct StoredData {
    /// All managed sessions, keyed by stringified id.
    #[serde(default)]
    sessions: std::collections::HashMap<String, SessionView>,
}

/// Canonical location of the managed-session store.
///
/// Why: one definition, so the two tools cannot disagree about where state
/// lives, and so a test can compare against the same expression production
/// uses instead of re-deriving it.
/// What: `~/.trusty-mpm/session-manager/sessions.json`, or `None` when the
/// platform reports no home directory.
/// Test: `default_store_path_is_under_home`.
pub(super) fn default_store_path() -> Option<PathBuf> {
    Some(
        dirs::home_dir()?
            .join(".trusty-mpm")
            .join("session-manager")
            .join("sessions.json"),
    )
}

/// Read every managed-session record from `path`, most-recently-active first.
///
/// Why: The tools need one read helper with one error policy so their output
/// is predictable. The policy distinguishes the two failure modes that mean
/// different things to a caller: a store that does not exist yet (no
/// orchestration harness has ever run here — legitimately "no sessions", not
/// a fault) versus a store that exists but cannot be parsed (real corruption,
/// which must be reported rather than silently rendered as an empty list).
/// What: an absent file yields `Ok(vec![])`. A present file yields its
/// records sorted by [`SessionView::activity_key`] descending, with the map
/// key used as `id` whenever the record's own `id` field is empty (older
/// writers keyed the map without repeating the id inside). An unreadable or
/// unparseable file yields `Err`.
/// Test: `load_sessions_reads_records_and_sorts_by_activity`,
/// `load_sessions_absent_store_is_empty_not_an_error`,
/// `load_sessions_malformed_json_is_an_error`.
pub(super) fn load_sessions(path: &Path) -> anyhow::Result<Vec<SessionView>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("cannot read session store {}: {e}", path.display()))?;
    let data: StoredData = serde_json::from_str(&raw)
        .map_err(|e| anyhow::anyhow!("session store {} is not valid JSON: {e}", path.display()))?;
    let mut out: Vec<SessionView> = data
        .sessions
        .into_iter()
        .map(|(key, mut view)| {
            if view.id.is_empty() {
                view.id = key;
            }
            view
        })
        .collect();
    out.sort_by(|a, b| b.activity_key().cmp(a.activity_key()));
    Ok(out)
}
