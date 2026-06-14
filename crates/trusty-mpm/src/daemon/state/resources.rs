//! Resource-management methods on [`DaemonState`].
//!
//! Why: circuit breakers, memory usage, trusty sidecar addresses, the token
//! optimizer, overseer, audit logger, and hook-event ring buffer are distinct
//! resource types but all live on `DaemonState`; grouping them here keeps this
//! module focused and under the SLOC cap.
//! What: breakers, memory, trusty_addrs, optimizer, overseer, audit, and
//! hook-event methods.
//! Test: see `super::tests`.

use std::sync::Arc;

use crate::core::circuit::CircuitBreaker;
use crate::core::hook::HookEventRecord;
use crate::core::memory::{MemoryPressure, MemoryUsage};
use crate::core::overseer::Overseer;
use crate::core::session::SessionId;

use crate::daemon::audit::AuditLogger;

use super::core::{DaemonState, HOOK_HISTORY_LIMIT};
use super::overseer::load_optimizer_config;

impl DaemonState {
    // ---- circuit breakers ----------------------------------------------

    /// Get a snapshot of an agent's circuit breaker, creating a closed one if
    /// the agent has not been seen before.
    pub fn breaker(&self, agent: &str) -> CircuitBreaker {
        self.breakers
            .entry(agent.to_string())
            .or_insert_with(|| CircuitBreaker::new(self.circuit_config))
            .value()
            .clone()
    }

    /// Record a delegation outcome against an agent's breaker.
    ///
    /// Why: the daemon must update breaker state after every delegation so the
    /// next `agent_delegate` call is gated correctly.
    /// What: success/failure drives `record_success` / `record_failure`.
    /// Test: `breaker_tracks_outcomes`.
    pub fn record_outcome(&self, agent: &str, success: bool) {
        let mut entry = self
            .breakers
            .entry(agent.to_string())
            .or_insert_with(|| CircuitBreaker::new(self.circuit_config));
        if success {
            entry.record_success();
        } else {
            entry.record_failure();
        }
    }

    /// Snapshot every known agent's circuit breaker.
    pub fn all_breakers(&self) -> Vec<(String, CircuitBreaker)> {
        self.breakers
            .iter()
            .map(|e| (e.key().clone(), e.value().clone()))
            .collect()
    }

    // ---- memory ---------------------------------------------------------

    /// Record a token-usage snapshot and classify the resulting pressure.
    ///
    /// Why: the MCP `memory_protect` tool and `TokenUsageUpdate` hooks both
    /// feed usage in; the daemon stores it and returns the pressure level so
    /// the caller (and dashboard) know whether to warn/alert/compact.
    /// What: stores `usage` for the session, returns `usage.pressure(config)`.
    /// Test: `memory_pressure_is_classified`.
    pub fn record_memory(&self, session: SessionId, usage: MemoryUsage) -> MemoryPressure {
        self.memory.insert(session, usage);
        usage.pressure(&self.memory_config)
    }

    /// Latest memory usage for a session, if any has been recorded.
    pub fn memory_for(&self, session: SessionId) -> Option<MemoryUsage> {
        self.memory.get(&session).map(|e| *e.value())
    }

    // ---- trusty sidecar discovery --------------------------------------

    /// Record the trusty sidecar addresses discovered at daemon startup.
    ///
    /// Why: discovery runs once when the HTTP daemon boots; the resolved
    /// addresses must be visible to request handlers that proxy to the
    /// trusty-memory / trusty-search sidecars.
    /// What: stores the `TrustyAddrs` snapshot under the mutex.
    /// Test: `trusty_addrs_round_trip`.
    pub fn set_trusty_addrs(&self, addrs: crate::daemon::discover::TrustyAddrs) {
        *self.trusty_addrs.lock() = Some(addrs);
    }

    /// Read the discovered trusty sidecar addresses, if discovery has run.
    ///
    /// Why: handlers need the resolved addresses; `None` means discovery has
    /// not completed (e.g. in MCP mode, which skips it).
    /// What: returns a clone of the stored `TrustyAddrs`.
    /// Test: `trusty_addrs_round_trip`.
    #[allow(dead_code)] // Read by sidecar-proxy handlers landing in a follow-up.
    pub fn trusty_addrs(&self) -> Option<crate::daemon::discover::TrustyAddrs> {
        self.trusty_addrs.lock().clone()
    }

    // ---- token-use optimizer -------------------------------------------

    /// Snapshot the current optimizer configuration.
    ///
    /// Why: the PostToolUse hook path reads this on every event; cloning a
    /// small struct under a short read lock keeps the hot path lock-free
    /// during compression itself.
    /// What: returns a clone of the stored `OptimizerConfig`.
    /// Test: `get_optimizer_returns_default`.
    pub fn optimizer_config(&self) -> crate::daemon::optimizer::OptimizerConfig {
        self.optimizer.read().clone()
    }

    /// Re-read the optimizer policy from the installed framework on disk.
    ///
    /// Why: the policy file is framework-managed and edited directly (or reset
    /// via `trusty-mpm install --force`); the file watcher calls this when
    /// `optimizer.toml` changes so the running daemon picks up edits without a
    /// restart.
    /// What: reloads `~/.trusty-mpm/framework/hooks/optimizer.toml`, replacing
    /// the in-memory config under a write lock. A missing or malformed file
    /// falls back to `OptimizerConfig::default()` (logged, not fatal).
    /// Test: `reload_optimizer_config_picks_up_file_changes`.
    pub fn reload_optimizer_config(&self) {
        *self.optimizer.write() = load_optimizer_config();
    }

    /// Reload the optimizer policy from an explicit file path.
    ///
    /// Why: tests must exercise the reload path against a temp file without
    /// touching the real `~/.trusty-mpm` framework install.
    /// What: loads `path` via [`OptimizerConfig::load_from_file`] and stores the
    /// result; a missing file yields `OptimizerConfig::default()`.
    /// Test: `reload_optimizer_config_picks_up_file_changes`.
    pub fn reload_optimizer_config_from(&self, path: &std::path::Path) -> anyhow::Result<()> {
        let cfg = crate::daemon::optimizer::OptimizerConfig::load_from_file(path)?;
        *self.optimizer.write() = cfg;
        Ok(())
    }

    // ---- overseer -------------------------------------------------------

    /// The session overseer for evaluating hook events.
    ///
    /// Why: the hook relay consults the overseer on tool-use events; handing
    /// out the shared `Arc` keeps every call site using the one configured
    /// strategy.
    /// What: returns a clone of the `Arc<dyn Overseer>`.
    /// Test: `overseer_is_accessible`.
    pub fn overseer(&self) -> Arc<dyn Overseer> {
        Arc::clone(&self.overseer)
    }

    /// Name of the active overseer strategy.
    ///
    /// Why: `GET /overseer` and the audit log report which strategy is in
    /// force; the name is fixed at construction so callers need no config.
    /// What: returns `"deterministic"` or `"composite-llm"`.
    /// Test: `overseer_handler_reports_strategy`.
    pub fn overseer_handler(&self) -> &str {
        &self.overseer_handler
    }

    /// The overseer audit logger.
    ///
    /// Why: the hook relay logs every overseer decision; sharing the `Arc`
    /// keeps all decisions flowing into the one dated JSONL file.
    /// What: returns a clone of the `Arc<AuditLogger>`.
    /// Test: `audit_logger_is_accessible`.
    pub fn audit(&self) -> Arc<AuditLogger> {
        Arc::clone(&self.audit)
    }

    /// The standalone LLM overseer for interactive chat, if configured.
    ///
    /// Why: `POST /llm/chat` needs the concrete [`LlmOverseer`] (the hook-path
    /// overseer is hidden behind `dyn Overseer`); this is `Some` exactly when an
    /// OpenRouter API key resolved at startup.
    /// What: returns a clone of the `Arc<LlmOverseer>`, or `None` when LLM chat
    /// is not configured.
    /// Test: `llm_overseer_is_none_without_key`.
    pub fn llm_overseer(&self) -> Option<Arc<crate::daemon::llm_overseer::LlmOverseer>> {
        self.llm.clone()
    }

    // ---- hook events ----------------------------------------------------

    /// Append a hook event to the bounded history ring buffer and broadcast it.
    ///
    /// Why: the dashboard's live feed reads recent events; the buffer must not
    /// grow without bound in a long-running daemon. The same call also fans
    /// the event out to every active SSE subscriber so push consumers (the
    /// GUI) see the event in real time without polling.
    /// What: pushes to the back, evicting the oldest once `HOOK_HISTORY_LIMIT`
    /// is exceeded, then best-effort broadcasts a JSON copy of the record to
    /// `event_tx`. Send errors (no active subscribers) are intentionally
    /// ignored.
    /// Test: `hook_history_is_bounded`, `ingest_hook_broadcasts_to_subscribers`.
    pub fn push_hook_event(&self, record: HookEventRecord) {
        let value = serde_json::to_value(&record).unwrap_or_default();
        {
            let mut buf = self.hook_history.lock();
            if buf.len() >= HOOK_HISTORY_LIMIT {
                buf.pop_front();
            }
            buf.push_back(record);
        }
        // Best-effort broadcast: a send error means no active subscribers,
        // which is the common case and not a fault.
        let _ = self.event_tx.send(value);
    }

    /// Subscribe to the live hook-event broadcast stream.
    ///
    /// Why: SSE handlers need a fresh receiver per connection. Wrapping the
    /// raw `Sender::subscribe` call in a method keeps the channel field's
    /// access pattern uniform and documents the intended consumer.
    /// What: returns a new `broadcast::Receiver` that will see every event
    /// published after `subscribe` was called, up to `EVENT_CHANNEL_CAPACITY`
    /// of backlog before lagging.
    /// Test: `ingest_hook_broadcasts_to_subscribers`.
    pub fn event_subscribe(&self) -> tokio::sync::broadcast::Receiver<serde_json::Value> {
        self.event_tx.subscribe()
    }

    /// Snapshot recent hook events, newest last.
    pub fn recent_hook_events(&self) -> Vec<HookEventRecord> {
        self.hook_history.lock().iter().cloned().collect()
    }

    /// Recent hook events for one session only.
    pub fn hook_events_for(&self, session: SessionId) -> Vec<HookEventRecord> {
        self.hook_history
            .lock()
            .iter()
            .filter(|r| r.session == session)
            .cloned()
            .collect()
    }
}
