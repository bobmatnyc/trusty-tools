//! CTRL state container and PM actor lifecycle.
//!
//! Why: The interactive REPL needs to own a collection of PM session handles,
//! along with shared slots (memory, active project, docs index, user profile)
//! that tools mutate during a turn. Pulling this state into its own module
//! keeps `mod.rs` focused on re-exports and lets the dispatch/repl/handlers
//! modules import only the types they actually touch.
//! What: `ConversationTurn`, `PmMsg`, `PmHandle`, `Ctrl`, and the
//! `pm_actor_task` background loop.
//! Test: `ctrl::tests::*` covers Ctrl::new / prompt / connect / disconnect /
//! status / dispatch_task / shutdown_all directly.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use tokio::sync::{mpsc, oneshot};

use crate::bus::MessageBus;
use crate::registry::ProjectRegistry;

use super::pm_task::run_pm_task;

/// One turn in a multi-turn conversation: user message + assistant reply.
///
/// Why: The REPL needs to keep a running transcript so each new task carries
/// the prior dialog forward — without this, every PM call is stateless and
/// the LLM can't follow up on its own answers.
/// What: Two strings, the user input and the assistant response, in the order
/// they were exchanged. The REPL owns a `Vec<ConversationTurn>` and forwards
/// it to `run_pm_task_with_history` on every task submission.
/// Test: `repl::tests::*` exercise the push/clear semantics; the conversion
/// to chat messages is covered indirectly by `ctrl_history_builds_messages`.
#[derive(Debug, Clone)]
pub struct ConversationTurn {
    pub user: String,
    pub assistant: String,
}

/// Message sent from CTRL to a PM actor task.
pub(crate) enum PmMsg {
    /// Dispatch a user task; reply with the PM's response or an error.
    Task {
        text: String,
        reply: oneshot::Sender<Result<String>>,
    },
    /// Request graceful shutdown of the PM actor loop.
    Shutdown,
}

/// Handle to a running PM actor background task.
pub(crate) struct PmHandle {
    /// Short display name derived from the last path component.
    pub(crate) name: String,
    /// Absolute path of the project this PM manages.
    pub(crate) project_path: PathBuf,
    /// Channel to send messages to the PM actor loop.
    pub(crate) tx: mpsc::Sender<PmMsg>,
    /// JoinHandle so CTRL can await clean shutdown.
    pub(crate) task: tokio::task::JoinHandle<()>,
    /// #185: Latest status string ("running" | "idle" | "error").
    /// Why: The Taskmaster persona needs `task_status()` to report PM state.
    pub(crate) status: Arc<Mutex<String>>,
    /// #185: Last message exchanged with this PM (truncated). Empty until
    /// first dispatch.
    pub(crate) last_message: Arc<Mutex<String>>,
}

/// CTRL state — all currently connected PM sessions.
pub(crate) struct Ctrl {
    /// Keyed by canonical project path string.
    pub(crate) pms: HashMap<String, PmHandle>,
    /// Key of the currently focused PM (None = CTRL-level).
    pub(crate) active: Option<String>,
    /// #117: CTRL's own message bus handle for inter-project relay.
    /// `None` until `run_ctrl` calls `MessageBus::start`.
    pub(crate) bus: Option<Arc<MessageBus>>,
    /// Shared lookup of connected PM senders, keyed by project basename
    /// (matching `BusEnvelope::target_project`). The bus relay task uses
    /// this to forward a `task`-typed envelope into the PM actor's channel.
    /// Why: The relay runs in its own spawned task without access to
    /// `ctrl.pms`; sharing only the mpsc senders keeps coupling minimal
    /// while letting CTRL keep authoritative ownership of `PmHandle`.
    pub(crate) connected_pms: Arc<tokio::sync::Mutex<HashMap<String, mpsc::Sender<PmMsg>>>>,
    /// Shared in-memory fallback for memory_store / memory_recall when the
    /// embedded memory store is not reachable from the CTRL subprocess.
    ///
    /// Why: CTRL is a top-level REPL — we don't want memory ops to hard-fail
    /// when the user hasn't set up MCP. An in-memory Vec is good enough for
    /// the current session and stays small.
    /// What: `Arc<Mutex<Vec<String>>>` so the `MemoryTools` closure clones
    /// can mutate it safely.
    pub(crate) memory: Arc<Mutex<Vec<String>>>,
    /// Detected trusty-agents self-project root, when running from its own
    /// checkout. (#182)
    pub(crate) self_project: Option<PathBuf>,
    /// Lazily-built TF-IDF index over project documentation. (#187)
    ///
    /// Why: Lets the `search_docs` tool answer questions about trusty-agents
    /// configuration, agents, skills, and workflows without an LLM call.
    /// `Option` because the index is built in a background task; until it
    /// resolves, the tool returns a graceful "not ready" message.
    /// What: `Arc<Mutex<…>>` so the background builder can install the index
    /// after CTRL has already entered the REPL loop.
    pub(crate) docs_index: Arc<Mutex<Option<Arc<crate::docs_index::DocsIndex>>>>,
    /// Loaded user profile (`~/.trusty-agents/user.toml`). Injected into the CTRL
    /// system prompt so the LLM can personalize replies. (#193)
    pub(crate) user_profile: Option<crate::identity::user_profile::UserProfile>,
    /// (#202) Active project path used as a default when `start_pm` is
    /// invoked without an explicit `project_path` argument.
    ///
    /// Why: Lets the user say "start a PM" once they've called
    /// `set_active_project` without re-typing the path.
    /// What: `Arc<Mutex<Option<PathBuf>>>` so the `SetActiveProjectTool`
    /// closure can mutate it and `StartPmTool` can read it.
    pub(crate) active_project: Arc<Mutex<Option<PathBuf>>>,
}

impl Ctrl {
    // INTENT: Create empty CTRL state with no sessions.
    pub(crate) fn new() -> Self {
        Self {
            pms: HashMap::new(),
            active: None,
            bus: None,
            connected_pms: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            memory: Arc::new(Mutex::new(Vec::new())),
            self_project: None,
            docs_index: Arc::new(Mutex::new(None)),
            user_profile: None,
            active_project: Arc::new(Mutex::new(None)),
        }
    }

    // INTENT: Render shell-style prompt reflecting the active PM or CTRL level.
    pub(crate) fn prompt(&self) -> String {
        match &self.active {
            None => "CTRL> ".to_string(),
            Some(key) => {
                let name = self.pms.get(key).map(|h| h.name.as_str()).unwrap_or("?");
                format!("PM[{name}]> ")
            }
        }
    }

    // INTENT: Connect to a project directory, spawning a new PM actor if needed.
    pub(crate) async fn connect(&mut self, raw_path: &str) -> Result<String> {
        let project_path = PathBuf::from(raw_path)
            .canonicalize()
            .with_context(|| format!("cannot resolve path: {raw_path}"))?;
        let key = project_path.to_string_lossy().to_string();

        if self.pms.contains_key(&key) {
            self.active = Some(key.clone());
            let name = &self.pms[&key].name;
            return Ok(format!("Switched to existing PM[{name}]"));
        }

        let name = project_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| key.clone());

        let (tx, rx) = mpsc::channel::<PmMsg>(16);
        let task = tokio::spawn(pm_actor_task(project_path.clone(), rx));

        // Record the connect in the global projects.json so CTRL's
        // `list_projects` tool and the user's own history stay accurate.
        // Non-fatal: registry failures don't prevent the session from
        // starting — the user can still work with the PM.
        if let Ok(reg) = ProjectRegistry::new()
            && let Err(e) = reg.register_pm_start(&project_path).await
        {
            tracing::warn!(error = %e, "register_pm_start failed");
        }

        self.pms.insert(
            key.clone(),
            PmHandle {
                name: name.clone(),
                project_path,
                tx: tx.clone(),
                task,
                status: Arc::new(Mutex::new("idle".to_string())),
                last_message: Arc::new(Mutex::new(String::new())),
            },
        );
        // Register in the relay lookup keyed by basename so inbound
        // `BusEnvelope::target_project` values can find this PM.
        {
            let mut m = self.connected_pms.lock().await;
            m.insert(name.clone(), tx);
        }
        self.active = Some(key);
        Ok(format!("Connected to PM[{name}]"))
    }

    // INTENT: Clear active PM focus without stopping the actor.
    pub(crate) fn disconnect(&mut self) -> String {
        let msg = if let Some(key) = &self.active {
            let name = self.pms.get(key).map(|h| h.name.as_str()).unwrap_or("?");
            format!("Disconnected from PM[{name}] (still running in background)")
        } else {
            "No active PM session.".to_string()
        };
        self.active = None;
        msg
    }

    // INTENT: List all PM sessions with an active marker.
    pub(crate) fn status(&self) -> String {
        if self.pms.is_empty() {
            return "No PM sessions.".to_string();
        }
        let mut lines = vec!["PM sessions:".to_string()];
        for (key, handle) in &self.pms {
            let marker = if self.active.as_deref() == Some(key) {
                "*"
            } else {
                " "
            };
            lines.push(format!(
                "[{marker}] {}  {}",
                handle.name,
                handle.project_path.display()
            ));
        }
        lines.join("\n")
    }

    // INTENT: Send a task to the active PM actor and await the response.
    pub(crate) async fn dispatch_task(&mut self, text: String) -> Result<String> {
        let key = self
            .active
            .as_ref()
            .context("no active PM session — use /connect <PATH>")?
            .clone();
        let handle = self
            .pms
            .get(&key)
            .context("active PM session not found in map")?;
        // #185: Track PM status + last message for `task_status` tool.
        let status_arc = handle.status.clone();
        let last_arc = handle.last_message.clone();
        if let Ok(mut s) = status_arc.lock() {
            *s = "running".to_string();
        }
        if let Ok(mut m) = last_arc.lock() {
            let preview: String = text.chars().take(200).collect();
            *m = preview;
        }
        let (reply_tx, reply_rx) = oneshot::channel();
        handle
            .tx
            .send(PmMsg::Task {
                text,
                reply: reply_tx,
            })
            .await
            .context("PM actor channel closed")?;
        let result = reply_rx.await.context("PM actor reply channel dropped")?;
        // #185: Update status based on result.
        if let Ok(mut s) = status_arc.lock() {
            *s = match &result {
                Ok(_) => "idle".to_string(),
                Err(_) => "error".to_string(),
            };
        }
        result
    }

    // INTENT: Gracefully shut down all PM actors with a 5s timeout each.
    pub(crate) async fn shutdown_all(self) {
        for (key, handle) in self.pms {
            let _ = handle.tx.send(PmMsg::Shutdown).await;
            if let Err(e) =
                tokio::time::timeout(std::time::Duration::from_secs(5), handle.task).await
            {
                tracing::warn!(pm = %key, "PM actor did not shut down in 5s: {e}");
            }
        }
    }
}

// INTENT: Run the PM actor loop, processing tasks until shutdown.
pub(crate) async fn pm_actor_task(project_path: PathBuf, mut rx: mpsc::Receiver<PmMsg>) {
    tracing::info!(project = %project_path.display(), "PM actor starting");
    while let Some(msg) = rx.recv().await {
        match msg {
            PmMsg::Task { text, reply } => {
                let result = run_pm_task(&project_path, &text).await;
                let _ = reply.send(result);
            }
            PmMsg::Shutdown => break,
        }
    }
    tracing::info!(project = %project_path.display(), "PM actor stopped");
}
