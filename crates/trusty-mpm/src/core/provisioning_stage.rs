//! Discrete provisioning-stage events for the managed-spawn SSE stream (#1904).
//!
//! Why: `tm`'s first-run clone/provisioning blocks on one HTTP call while the
//! daemon does git clone, agent/skill deploy, instruction build, MCP config,
//! tmux creation, and runtime launch — previously the client had zero
//! visibility into which of those steps was in flight. This module gives the
//! sync provisioning code (`provisioner::workspace`, `core::session_launch`,
//! `daemon::managed_routes::lifecycle`) a way to announce stage transitions
//! WITHOUT taking a hard dependency on `daemon::DaemonState` — those modules
//! are shared by non-daemon callers (the TUI's `/connect`, `tm launch`
//! CLI-direct, and unit tests), so threading a `DaemonState` parameter through
//! every provisioning function would invert the `core`/`provisioner` →
//! `daemon` dependency direction. Instead, [`emit`] looks up a
//! [`tokio::task_local!`]-scoped [`StageEmitter`]; when no scope is active
//! (every caller except the daemon's `spawn_managed`) it is a silent no-op.
//! What: [`ProvisioningStage`] enumerates the coarse steps of a managed-session
//! spawn; [`StageEmitter`] carries the session id, `repo_url`, and a clone of
//! the daemon's existing SSE broadcast `Sender<serde_json::Value>`; [`scoped`]
//! runs a future with an emitter installed; [`emit`] publishes one stage
//! transition as a JSON envelope on that same broadcast channel — the exact
//! channel `GET /events` and `GET /sessions/{id}/events` already stream from
//! (`crate::daemon::state`), so no parallel event-delivery mechanism exists.
//! Test: `stage_wire_names_are_unique_and_round_trip`, `emit_outside_scope_is_noop`,
//! `emit_inside_scope_broadcasts_envelope`, `envelope_contains_repo_url_for_client_filtering`.

use std::future::Future;

use serde::{Deserialize, Serialize};

/// One coarse step of a managed-session spawn (issue #1904).
///
/// Why: the operator-visible ask was "show each main step: cloning, building
/// instructions, loading agents, skills, etc." — a small closed enum matching
/// the actual step boundaries in `provisioner::workspace::provision_in` and
/// `core::session_launch::prepare_session_inner` is enough; this is
/// deliberately NOT a generic progress-percentage system.
/// What: eight variants covering clone → agent/skill deploy → instructions →
/// MCP config → tmux creation → runtime launch → done.
/// Test: `stage_wire_names_are_unique_and_round_trip`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProvisioningStage {
    /// Cloning the repository into the session workspace.
    CloningRepo,
    /// Deploying composed agents to `~/.claude/agents/`.
    DeployingAgents,
    /// Deploying skill files to `~/.claude/skills/`.
    DeployingSkills,
    /// Composing the project `CLAUDE.md` / launch instructions.
    BuildingInstructions,
    /// Injecting the trusty-memory / trusty-search MCP server config.
    ConfiguringMcp,
    /// Creating the tmux host session.
    CreatingTmuxSession,
    /// Spawning the runtime (`claude`/`tcode`) in the tmux pane.
    LaunchingRuntime,
    /// Provisioning finished; the session is ready to attach.
    Complete,
}

impl ProvisioningStage {
    /// Stable wire identifier used in the SSE JSON envelope's `"stage"` field.
    ///
    /// Why: a stable string (rather than relying on `Debug`) keeps the wire
    /// contract independent of variant renames/reordering.
    /// What: returns the PascalCase variant name.
    /// Test: `stage_wire_names_are_unique_and_round_trip`.
    pub fn wire_name(&self) -> &'static str {
        match self {
            ProvisioningStage::CloningRepo => "CloningRepo",
            ProvisioningStage::DeployingAgents => "DeployingAgents",
            ProvisioningStage::DeployingSkills => "DeployingSkills",
            ProvisioningStage::BuildingInstructions => "BuildingInstructions",
            ProvisioningStage::ConfiguringMcp => "ConfiguringMcp",
            ProvisioningStage::CreatingTmuxSession => "CreatingTmuxSession",
            ProvisioningStage::LaunchingRuntime => "LaunchingRuntime",
            ProvisioningStage::Complete => "Complete",
        }
    }

    /// Parse a stage back from its [`wire_name`](Self::wire_name).
    ///
    /// Why: the client-side SSE watcher (`tm`'s `guided_launch` module)
    /// deserializes the daemon's JSON envelope and needs the inverse of
    /// `wire_name` to render a human label.
    /// What: returns `None` for an unrecognized string.
    /// Test: `stage_wire_names_are_unique_and_round_trip`.
    pub fn from_wire(name: &str) -> Option<ProvisioningStage> {
        Self::ALL.iter().copied().find(|s| s.wire_name() == name)
    }

    /// Every stage, in the order a normal clone-based spawn emits them.
    pub const ALL: [ProvisioningStage; 8] = [
        ProvisioningStage::CloningRepo,
        ProvisioningStage::DeployingAgents,
        ProvisioningStage::DeployingSkills,
        ProvisioningStage::BuildingInstructions,
        ProvisioningStage::ConfiguringMcp,
        ProvisioningStage::CreatingTmuxSession,
        ProvisioningStage::LaunchingRuntime,
        ProvisioningStage::Complete,
    ];

    /// Human-readable label for terminal/spinner display.
    ///
    /// Why: shared by the daemon (log context) and the `tm` CLI (spinner
    /// message) so the two never drift on wording.
    /// What: a short present-participle phrase, no trailing punctuation.
    /// Test: exercised via `envelope_contains_repo_url_for_client_filtering`
    /// (asserts every stage produces a non-empty label).
    pub fn label(&self) -> &'static str {
        match self {
            ProvisioningStage::CloningRepo => "Cloning repository",
            ProvisioningStage::DeployingAgents => "Deploying agents",
            ProvisioningStage::DeployingSkills => "Deploying skills",
            ProvisioningStage::BuildingInstructions => "Building instructions",
            ProvisioningStage::ConfiguringMcp => "Configuring MCP servers",
            ProvisioningStage::CreatingTmuxSession => "Creating session",
            ProvisioningStage::LaunchingRuntime => "Launching runtime",
            ProvisioningStage::Complete => "Done",
        }
    }
}

/// Carries the identity + broadcast channel [`emit`] needs to publish a stage
/// transition, scoped to one in-flight `spawn_managed` call.
///
/// Why: `core`/`provisioner` code must never import `daemon::DaemonState`
/// (see module doc); this struct is the minimal, daemon-agnostic subset —
/// just the session id (for the existing `/sessions/{id}/events` substring
/// filter), the `repo_url` (for the client's pre-session-id correlation,
/// since the client doesn't learn the session id until the whole spawn
/// completes), and a clone of the broadcast sender.
/// What: three fields, all cheap to clone (`String`s and a `broadcast::Sender`
/// clone, which is a cheap `Arc`-backed handle).
/// Test: `emit_inside_scope_broadcasts_envelope`.
#[derive(Clone)]
pub struct StageEmitter {
    session_id: String,
    repo_url: String,
    sender: tokio::sync::broadcast::Sender<serde_json::Value>,
}

impl StageEmitter {
    /// Why: constructing the envelope inputs once at `spawn_managed`'s entry
    /// keeps every `emit` call site simple (no repeated argument threading).
    /// What: stores the session id, repo URL, and broadcast sender by value.
    /// Test: `emit_inside_scope_broadcasts_envelope`.
    pub fn new(
        session_id: impl Into<String>,
        repo_url: impl Into<String>,
        sender: tokio::sync::broadcast::Sender<serde_json::Value>,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            repo_url: repo_url.into(),
            sender,
        }
    }
}

tokio::task_local! {
    /// The active [`StageEmitter`] for the currently-executing provisioning
    /// call, if any. Installed by [`scoped`], read by [`emit`].
    static CURRENT: StageEmitter;
}

/// Run `fut` with `emitter` installed as the active [`StageEmitter`].
///
/// Why: `daemon::managed_routes::lifecycle::spawn_managed` is the one caller
/// that has both a session id and the daemon's broadcast channel; wrapping
/// its provisioning body in this scope is the single integration point that
/// makes every `emit()` call inside the (synchronous) provisioning call chain
/// — including nested `.await`ed async fns — see the installed emitter,
/// without changing any of those functions' signatures.
/// What: delegates to `tokio::task_local::LocalKey::scope`. Any `core`/
/// `provisioner` caller that never wraps itself in `scoped` (the TUI's
/// `/connect`, CLI-direct `tm launch`, all unit tests) simply never has a
/// `CURRENT` value set, so `emit` inside those call paths is a no-op.
/// Test: `emit_inside_scope_broadcasts_envelope`, `emit_outside_scope_is_noop`.
pub async fn scoped<F: Future>(emitter: StageEmitter, fut: F) -> F::Output {
    CURRENT.scope(emitter, fut).await
}

/// Publish a stage transition on the active [`StageEmitter`]'s channel, if any.
///
/// Why: this is the ONLY seam the provisioning code (`provision_in`,
/// `prepare_session_inner`, `spawn_managed`) needs to call — it is
/// intentionally infallible and side-effect-free when no daemon SSE
/// subscriber context is active, so sprinkling `emit(...)` calls through the
/// existing best-effort provisioning steps can never turn a previously
/// non-fatal step into a fatal one, and never blocks (broadcast `send` is
/// synchronous and non-blocking; a full channel or zero subscribers both
/// degrade gracefully — see [`crate::daemon::state`]'s `push_hook_event` for
/// the identical pattern this mirrors).
/// What: builds `{"kind":"provisioning_stage","session":<id>,"repo_url":<url>,
/// "stage":<wire_name>,"label":<label>,"at":<rfc3339>}` and best-effort sends
/// it on the scoped emitter's broadcast sender. A missing scope, or a send
/// with zero active subscribers, are both silently ignored.
/// Test: `emit_inside_scope_broadcasts_envelope`, `emit_outside_scope_is_noop`,
/// `envelope_contains_repo_url_for_client_filtering`.
pub fn emit(stage: ProvisioningStage) {
    let _ = CURRENT.try_with(|emitter| {
        let value = serde_json::json!({
            "kind": "provisioning_stage",
            "session": emitter.session_id,
            "repo_url": emitter.repo_url,
            "stage": stage.wire_name(),
            "label": stage.label(),
            "at": chrono::Utc::now().to_rfc3339(),
        });
        let _ = emitter.sender.send(value);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the wire contract must be stable and collision-free — a duplicate
    /// or unparseable name would silently break client-side stage rendering.
    /// What: round-trips every `ALL` variant through `wire_name`/`from_wire`
    /// and asserts uniqueness.
    /// Test: this is the test.
    #[test]
    fn stage_wire_names_are_unique_and_round_trip() {
        let mut seen = std::collections::HashSet::new();
        for stage in ProvisioningStage::ALL {
            assert!(seen.insert(stage.wire_name()), "duplicate wire name");
            assert_eq!(ProvisioningStage::from_wire(stage.wire_name()), Some(stage));
            assert!(!stage.label().is_empty());
        }
        assert_eq!(ProvisioningStage::from_wire("NotAStage"), None);
    }

    /// Why: `emit` must never panic or do anything observable when called
    /// from a context that never entered `scoped` — this is the common case
    /// (every non-daemon caller of the shared provisioning code).
    /// What: calls `emit` with no active scope; the test passing (no panic)
    /// is the assertion.
    /// Test: this is the test.
    #[tokio::test]
    async fn emit_outside_scope_is_noop() {
        emit(ProvisioningStage::CloningRepo);
    }

    /// Why: this is the core integration the daemon relies on — a stage
    /// emitted from inside `scoped` must reach a subscriber of the passed-in
    /// broadcast sender, using the SAME envelope shape the client parses.
    /// What: subscribes to a fresh broadcast channel, emits two stages inside
    /// `scoped`, and asserts both envelopes arrive in order with the right
    /// session/repo_url/stage fields.
    /// Test: this is the test.
    #[tokio::test]
    async fn emit_inside_scope_broadcasts_envelope() {
        let (tx, mut rx) = tokio::sync::broadcast::channel(8);
        let emitter = StageEmitter::new("11111111-1111-1111-1111-111111111111", "owner/repo", tx);

        scoped(emitter, async {
            emit(ProvisioningStage::CloningRepo);
            emit(ProvisioningStage::Complete);
        })
        .await;

        let first = rx.try_recv().expect("first stage event");
        assert_eq!(first["kind"], "provisioning_stage");
        assert_eq!(first["session"], "11111111-1111-1111-1111-111111111111");
        assert_eq!(first["repo_url"], "owner/repo");
        assert_eq!(first["stage"], "CloningRepo");
        assert_eq!(first["label"], "Cloning repository");

        let second = rx.try_recv().expect("second stage event");
        assert_eq!(second["stage"], "Complete");

        assert!(rx.try_recv().is_err(), "no further events expected");
    }

    /// Why: the client cannot know the session id before the spawn POST
    /// returns (that's the whole point of the stretch goal — progress DURING
    /// the blocking call), so it correlates on `repo_url` against the GLOBAL
    /// `/events` stream instead of the per-session stream. Every envelope
    /// must therefore carry a plain, substring-matchable `repo_url`.
    /// What: asserts the envelope's `repo_url` round-trips exactly and every
    /// stage has a distinct label.
    /// Test: this is the test.
    #[tokio::test]
    async fn envelope_contains_repo_url_for_client_filtering() {
        let (tx, mut rx) = tokio::sync::broadcast::channel(16);
        let emitter = StageEmitter::new("s-1", "https://github.com/acme/widgets", tx);

        scoped(emitter, async {
            for stage in ProvisioningStage::ALL {
                emit(stage);
            }
        })
        .await;

        let mut labels = std::collections::HashSet::new();
        for _ in ProvisioningStage::ALL {
            let val = rx.try_recv().expect("stage event");
            assert_eq!(val["repo_url"], "https://github.com/acme/widgets");
            labels.insert(val["label"].as_str().unwrap().to_string());
        }
        assert_eq!(
            labels.len(),
            ProvisioningStage::ALL.len(),
            "labels must be distinct"
        );
    }
}
