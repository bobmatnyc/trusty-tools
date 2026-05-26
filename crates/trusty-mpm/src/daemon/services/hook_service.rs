//! Hook processing pipeline business logic.
//!
//! Why: the `POST /hooks` handler embedded the overseer context construction,
//! the event-kind dispatch, the audit write, and the `PostToolUse` compression
//! decision. That is the daemon's enforcement core; isolating it in a service
//! makes each step testable and replaces the free `run_overseer` /
//! `overseer_context` functions that lived in `api.rs`.
//! What: [`HookDecision`] is the daemon-facing verdict; [`HookService`] builds
//! the [`OverseerContext`] from a raw payload, consults the configured
//! overseer, audits the verdict, applies output optimization, and records the
//! event in the ring buffer. [`FileChanged`](HookEvent::FileChanged) events are
//! pre-filtered by [`is_coding_file`] so OS / browser noise never enters the
//! ring buffer.
//! Test: `cargo test -p trusty-mpm-daemon services::hook` covers the
//! disabled-overseer fast path, the decision conversion, and the file filter.

use crate::core::hook::{HookEvent, HookEventRecord};
use crate::core::overseer::{OverseerContext, OverseerDecision};
use crate::core::session::SessionId;
use serde_json::Value;

use crate::daemon::audit::AuditEntry;
use crate::daemon::state::DaemonState;

// ---- FileChanged noise filter -------------------------------------------

/// Path substrings that indicate OS / browser noise rather than source files.
///
/// Why: Claude Code's `FileChanged` hook fires for every `inotify`/FSEvents
/// notification on the system, including Chrome temp files, macOS preferences,
/// and browser caches. Storing those in the ring buffer fills the TUI "Recent
/// Events" panel with irrelevant noise.
/// What: a substring deny-list checked case-insensitively against the full
/// path.  Any match causes the event to be silently dropped before ring-buffer
/// insertion.
/// Test: `is_coding_file_rejects_noise`, `is_coding_file_accepts_source`.
const NOISE_PATTERNS: &[&str] = &[
    // Browser temp/state files
    ".com.google.chrome",
    ".com.apple.",
    "com.apple.",
    "preferences",
    "cookies",
    "history",
    "cache",
    "gpucache",
    "indexeddb",
    "localstorage",
    "sessionstorage",
    // OS noise
    ".ds_store",
    ".spotlight-",
    ".temporaryitems",
    ".trashes",
    ".fseventsd",
    // Log / tmp / lock
    ".log",
    ".tmp",
    ".lock",
    // Build / dependency artifacts
    "node_modules/",
    "/target/",
    "/.git/",
];

/// File extensions that identify coding-relevant files.
///
/// Why: an allow-list guards against path names that don't match any noise
/// pattern but are still irrelevant (e.g. `~/.zsh_history`). Files with one of
/// these extensions are always kept.
/// Test: `is_coding_file_accepts_source`.
const CODE_EXTENSIONS: &[&str] = &[
    ".rs", ".toml", ".ts", ".tsx", ".js", ".jsx", ".py", ".go", ".java", ".kt", ".swift", ".c",
    ".cpp", ".h", ".hpp", ".md", ".yaml", ".yml", ".json", ".sh", ".env", ".sql", ".html", ".css",
    ".scss", ".vue", ".svelte",
];

/// Source-directory path components that flag a file as project-related.
///
/// Why: some project files (e.g. `Makefile`, `.env`) may lack a recognised
/// extension but are clearly code when they live under a source tree.
/// Test: `is_coding_file_accepts_source_dir`.
const SOURCE_DIRS: &[&str] = &["/src/", "/lib/", "/crates/", "/packages/"];

/// Return `true` when a `FileChanged` path represents a coding-relevant file.
///
/// Why: OS file watchers emit events for every byte written anywhere on the
/// system; the ring buffer must only retain events that a developer would
/// recognise as meaningful (source, config, docs). Centralising the logic here
/// makes it easy to extend without touching the event dispatch loop.
/// What: rejects any path whose lowercased form contains a [`NOISE_PATTERNS`]
/// substring; keeps any path with a [`CODE_EXTENSIONS`] suffix or a
/// [`SOURCE_DIRS`] component; silently drops everything else.
/// Test: `is_coding_file_rejects_noise`, `is_coding_file_accepts_source`,
/// `is_coding_file_accepts_source_dir`.
fn is_coding_file(path: &str) -> bool {
    let lower = path.to_lowercase();

    // Deny: matches any noise pattern.
    if NOISE_PATTERNS.iter().any(|p| lower.contains(p)) {
        return false;
    }

    // Allow: has a recognised code extension.
    if CODE_EXTENSIONS.iter().any(|ext| lower.ends_with(ext)) {
        return true;
    }

    // Allow: lives inside a source directory tree.
    SOURCE_DIRS.iter().any(|dir| path.contains(dir))
}

/// The daemon-facing result of processing one hook event.
///
/// Why: [`OverseerDecision`] is the core's vocabulary; the daemon wants a verdict
/// it owns so the HTTP layer is decoupled from the core enum and can carry
/// daemon-specific follow-up (e.g. "this event was already recorded").
/// What: the four overseer outcomes, with the same data each carries.
/// Test: `decision_converts_from_overseer`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookDecision {
    /// Let the event proceed; it has been recorded.
    Allow,
    /// The overseer halted the event; `reason` explains why.
    Block {
        /// Human-readable explanation of the block.
        reason: String,
    },
    /// The overseer wants `text` injected into the session.
    Respond {
        /// Text to send into the session.
        text: String,
    },
    /// The overseer escalated the event for human review.
    FlagForHuman {
        /// Short description of why human attention is needed.
        summary: String,
    },
}

impl From<OverseerDecision> for HookDecision {
    /// Map a core overseer verdict onto the daemon's decision type.
    ///
    /// Why: the two enums are structurally identical; an explicit `From` keeps
    /// the conversion in one place instead of scattered `match`es.
    /// What: variant-for-variant translation.
    /// Test: `decision_converts_from_overseer`.
    fn from(d: OverseerDecision) -> Self {
        match d {
            OverseerDecision::Allow => Self::Allow,
            OverseerDecision::Block { reason } => Self::Block { reason },
            OverseerDecision::Respond { text } => Self::Respond { text },
            OverseerDecision::FlagForHuman { summary } => Self::FlagForHuman { summary },
        }
    }
}

impl HookDecision {
    /// Stable lowercase tag for this decision (`"allow" | "block" | ...`).
    pub fn tag(&self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Block { .. } => "block",
            Self::Respond { .. } => "respond",
            Self::FlagForHuman { .. } => "flag",
        }
    }

    /// The human-readable detail of this decision, if any.
    pub fn detail(&self) -> Option<&str> {
        match self {
            Self::Allow => None,
            Self::Block { reason } => Some(reason),
            Self::Respond { text } => Some(text),
            Self::FlagForHuman { summary } => Some(summary),
        }
    }
}

/// Hook event processing over the shared daemon state.
///
/// Why: a borrowed facade — the handler builds one per request and delegates
/// the whole relay pipeline to it, so `ingest_hook` shrinks to a few lines.
/// What: holds a borrow of [`DaemonState`]; [`process`](Self::process) runs the
/// overseer-audit-optimize-record pipeline for one event.
/// Test: the module's `#[cfg(test)]` suite.
pub struct HookService<'s> {
    state: &'s DaemonState,
}

impl<'s> HookService<'s> {
    /// Build a service bound to `state`.
    pub fn new(state: &'s DaemonState) -> Self {
        Self { state }
    }

    /// Process one hook event end to end.
    ///
    /// Why: this is the daemon's full hook pipeline — consult the overseer on
    /// tool-use events (auditing every verdict), compress `PostToolUse` output,
    /// then append the event to the ring buffer. Keeping it in one method makes
    /// the order of those steps explicit and testable.
    /// What: builds an [`OverseerContext`], runs the overseer when it is
    /// enabled, records the event (unless blocked), and returns the verdict. A
    /// `Block` short-circuits before the event is recorded.
    /// Test: `process_records_event_with_disabled_overseer`.
    pub fn process(
        &self,
        session: SessionId,
        event: HookEvent,
        mut payload: Value,
    ) -> HookDecision {
        // 1. Overseer: evaluate + audit tool-use events. Skipped entirely when
        //    oversight is disabled (the common opt-out path).
        let overseer = self.state.overseer();
        if overseer.is_enabled()
            && let Some(decision) = self.run_overseer(&overseer, event, session, &payload)
        {
            if let OverseerDecision::Block { reason } = &decision {
                return HookDecision::Block {
                    reason: reason.clone(),
                };
            }
            if let OverseerDecision::Respond { text } = &decision {
                tracing::info!("overseer auto-response for {session:?}: {text}");
            }
        }

        // 2. PostToolUse: compress tool output before it enters the ring buffer.
        if event == HookEvent::PostToolUse {
            let tool_name = payload
                .get("tool")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            let cfg = self.state.optimizer_config();
            crate::daemon::optimizer::optimize_tool_output(&cfg, &tool_name, &mut payload);
        }

        // 2b. FileChanged: drop OS / browser noise before it enters the ring
        //     buffer. Only coding-relevant paths (source files, config, docs)
        //     are kept. Non-FileChanged events are unaffected.
        if event == HookEvent::FileChanged {
            let path = payload.get("path").and_then(Value::as_str).unwrap_or("");
            if !is_coding_file(path) {
                tracing::trace!("dropped FileChanged noise: {path}");
                return HookDecision::Allow;
            }
        }

        // 3. Record the event in the bounded history.
        self.state
            .push_hook_event(HookEventRecord::now(session, event, payload));
        HookDecision::Allow
    }

    /// Build an [`OverseerContext`] from a raw hook payload.
    ///
    /// Why: the overseer evaluates events by tool name and input; extracting
    /// those from the opaque payload belongs in one place. Replaces the free
    /// `overseer_context` function from `api.rs`.
    /// What: resolves the session's friendly name (falling back to the UUID),
    /// reads `payload["tool"]` and serializes `payload["input"]`.
    /// Test: covered by `process_records_event_with_disabled_overseer`.
    fn context(&self, session: SessionId, payload: &Value) -> OverseerContext {
        let tmux_name = self
            .state
            .session(session)
            .map(|s| s.tmux_name)
            .unwrap_or_else(|| session.0.to_string());
        let tool_name = payload
            .get("tool")
            .and_then(Value::as_str)
            .map(str::to_string);
        let tool_input = payload
            .get("input")
            .map(|v| v.to_string())
            .or_else(|| Some(payload.to_string()));
        OverseerContext::new(session, tmux_name, tool_name, tool_input)
    }

    /// Run the overseer for one event and audit the verdict.
    ///
    /// Why: keeping the event-kind dispatch and the audit write in one helper
    /// keeps [`process`](Self::process) focused on the relay flow.
    /// What: maps `PreToolUse` / `PostToolUse` onto the matching overseer call,
    /// writes an [`AuditEntry`], and returns the decision; other events return
    /// `None` (the overseer does not act on them).
    /// Test: covered by `process_records_event_with_disabled_overseer`.
    fn run_overseer(
        &self,
        overseer: &std::sync::Arc<dyn crate::core::overseer::Overseer>,
        event: HookEvent,
        session: SessionId,
        payload: &Value,
    ) -> Option<OverseerDecision> {
        let ctx = self.context(session, payload);
        let (event_label, decision) = match event {
            HookEvent::PreToolUse => ("PreToolUse", overseer.pre_tool_use(&ctx)),
            HookEvent::PostToolUse => {
                let output = payload.get("output").and_then(Value::as_str).unwrap_or("");
                ("PostToolUse", overseer.post_tool_use(&ctx, output))
            }
            _ => return None,
        };
        self.state.audit().log(AuditEntry::from_decision(
            &ctx,
            event_label,
            &decision,
            self.state.overseer_handler(),
        ));
        Some(decision)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::session::{ControlModel, Session, SessionStatus};

    #[test]
    fn decision_converts_from_overseer() {
        assert_eq!(
            HookDecision::from(OverseerDecision::Allow),
            HookDecision::Allow
        );
        assert_eq!(
            HookDecision::from(OverseerDecision::Block { reason: "x".into() }),
            HookDecision::Block { reason: "x".into() }
        );
        assert_eq!(HookDecision::Allow.tag(), "allow");
        assert_eq!(
            HookDecision::Block { reason: "r".into() }.detail(),
            Some("r")
        );
    }

    #[test]
    fn process_records_event_with_disabled_overseer() {
        // With the overseer disabled (the default), a known event must be
        // recorded and the verdict is Allow.
        let state = DaemonState::new();
        let id = SessionId::new();
        let mut s = Session::new(id, "/tmp/p", ControlModel::Tmux, None);
        s.status = SessionStatus::Active;
        state.register_session(s);

        let svc = HookService::new(&state);
        let decision = svc.process(
            id,
            HookEvent::PreToolUse,
            serde_json::json!({ "tool": "Bash" }),
        );
        assert_eq!(decision, HookDecision::Allow);
        assert_eq!(state.recent_hook_events().len(), 1);
    }

    // ---- is_coding_file unit tests ---------------------------------------

    #[test]
    fn is_coding_file_rejects_noise() {
        // Browser temp files.
        assert!(!is_coding_file(
            "/var/folders/abc/.com.google.Chrome.xyz/Preferences"
        ));
        // macOS preference directories.
        assert!(!is_coding_file(
            "/Users/masa/Library/Preferences/com.apple.finder.plist"
        ));
        // DS_Store and spotlight noise.
        assert!(!is_coding_file("/Users/masa/Projects/.DS_Store"));
        assert!(!is_coding_file("/private/var/.Spotlight-V100/something"));
        // Log, tmp, lock files.
        assert!(!is_coding_file("/tmp/daemon.log"));
        assert!(!is_coding_file("/tmp/work.tmp"));
        assert!(!is_coding_file("/var/run/sshd.lock"));
        // Build / dependency artifacts.
        assert!(!is_coding_file(
            "/Users/masa/Projects/app/node_modules/lodash/index.js"
        ));
        assert!(!is_coding_file(
            "/Users/masa/Projects/trusty/target/debug/main"
        ));
        assert!(!is_coding_file(
            "/Users/masa/Projects/app/.git/objects/pack/pack-abc.idx"
        ));
    }

    #[test]
    fn is_coding_file_accepts_source() {
        // Rust, TOML, TypeScript, Python.
        assert!(is_coding_file(
            "/Users/masa/Projects/trusty/crates/core/src/lib.rs"
        ));
        assert!(is_coding_file("/Users/masa/Projects/trusty/Cargo.toml"));
        assert!(is_coding_file(
            "/Users/masa/Projects/app/src/components/Button.tsx"
        ));
        assert!(is_coding_file("/Users/masa/Projects/app/scripts/build.py"));
        // Config and docs.
        assert!(is_coding_file("/Users/masa/Projects/app/README.md"));
        assert!(is_coding_file(
            "/Users/masa/Projects/app/.github/workflows/ci.yaml"
        ));
        assert!(is_coding_file("/Users/masa/Projects/app/schema.sql"));
        assert!(is_coding_file("/Users/masa/Projects/app/.env"));
    }

    #[test]
    fn is_coding_file_accepts_source_dir() {
        // Files without a recognised extension but inside a source tree.
        assert!(is_coding_file("/Users/masa/Projects/app/src/Makefile"));
        assert!(is_coding_file(
            "/Users/masa/Projects/trusty/crates/daemon/src/BUILD"
        ));
        assert!(is_coding_file(
            "/Users/masa/Projects/app/packages/ui/Dockerfile"
        ));
        assert!(is_coding_file("/Users/masa/Projects/lib/core/something"));
    }

    #[test]
    fn is_coding_file_drops_unknown_extension_outside_source_dirs() {
        // A file with no recognised extension and not in a source dir.
        assert!(!is_coding_file("/Users/masa/Downloads/somefile.xyz"));
        // Shell history and similar home-dir clutter.
        assert!(!is_coding_file("/Users/masa/.zsh_history"));
    }

    #[test]
    fn file_changed_noise_is_not_recorded() {
        // A Chrome temp-file `FileChanged` event must be silently dropped and
        // must not appear in the ring buffer.
        let state = DaemonState::new();
        let id = SessionId::new();
        let mut s = Session::new(id, "/tmp/p", ControlModel::Tmux, None);
        s.status = SessionStatus::Active;
        state.register_session(s);

        let svc = HookService::new(&state);
        let decision = svc.process(
            id,
            HookEvent::FileChanged,
            serde_json::json!({
                "path": "/var/folders/abc/.com.google.Chrome.xyz/Preferences"
            }),
        );
        // The event is silently allowed (no error) but not recorded.
        assert_eq!(decision, HookDecision::Allow);
        assert_eq!(state.recent_hook_events().len(), 0);
    }

    #[test]
    fn file_changed_source_file_is_recorded() {
        // A Rust source file must pass through the filter and enter the ring
        // buffer unchanged.
        let state = DaemonState::new();
        let id = SessionId::new();
        let mut s = Session::new(id, "/tmp/p", ControlModel::Tmux, None);
        s.status = SessionStatus::Active;
        state.register_session(s);

        let svc = HookService::new(&state);
        let decision = svc.process(
            id,
            HookEvent::FileChanged,
            serde_json::json!({
                "path": "/Users/masa/Projects/trusty/crates/core/src/lib.rs"
            }),
        );
        assert_eq!(decision, HookDecision::Allow);
        assert_eq!(state.recent_hook_events().len(), 1);
    }

    #[test]
    fn non_file_changed_events_bypass_filter() {
        // Non-FileChanged events must always be recorded regardless of any
        // payload content — the filter is FileChanged-only.
        let state = DaemonState::new();
        let id = SessionId::new();
        let mut s = Session::new(id, "/tmp/p", ControlModel::Tmux, None);
        s.status = SessionStatus::Active;
        state.register_session(s);

        let svc = HookService::new(&state);
        let decision = svc.process(id, HookEvent::SessionStart, serde_json::json!({}));
        assert_eq!(decision, HookDecision::Allow);
        assert_eq!(state.recent_hook_events().len(), 1);
    }
}
