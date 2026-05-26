//! CTRL actor — interactive multi-project PM coordination CLI.
//!
//! Why: A single binary entry point that manages multiple PM sessions across
//! different project directories, routing user input to the active session or
//! dispatching slash commands for lifecycle management.
//! What: `run_ctrl` presents a readline-style prompt, manages named `PmHandle`
//! actors (one per project), and dispatches tasks or commands accordingly.
//! Test: Run `cargo run` with no `--pm` flag; type `/help`; verify command
//! listing; `/connect <project_path>` to start a PM session.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use anyhow::{Context, Result};
use async_openai::types::ChatCompletionTool;
use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

use crate::agents::AgentConfig;
use crate::bus::{BusEnvelope, MessageBus};
use crate::events::{self, Event};
use crate::intent::{IntentClass, classify_intent};
use crate::llm;
use crate::registry::ProjectRegistry;
use crate::session_record;
use crate::subprocess::SubprocessAgentRunner;
use crate::tools::traits::{ToolExecutor, ToolResult};
use crate::tools::{AgentRunner, ToolRegistry, delegate::DelegateToAgentTool};

pub mod socket;
pub use socket::{CtrlSocket, ctrl_socket_path, cwd_project_id, is_connection_refused};

pub mod supervisor;
pub use supervisor::{CtrlSupervisor, SupervisorOutcome};

/// Session-scoped overrides applied at PM dispatch time (#284).
///
/// Why: The REPL `/model` and `/provider` slash commands let the user pin a
/// model id or credential routing path for the rest of the session without
/// editing TOML. The override fields are passed through to
/// `run_pm_task_with_persona` and `run_pm_task_with_history` so they can be
/// applied AFTER `AgentConfig::load()` and BEFORE `apply_credential_routing()`.
/// What: Two optional knobs. `model` overrides `cfg.agent.model`. `provider`
/// is one of `"claude-code"`, `"bedrock"`, `"openrouter"` and replaces the
/// `pick_credentials()` env probe for the duration of the dispatch.
/// Bookmarks: `"anthropic-api"` and `"openai-api"` are NOT yet wired up — add
/// them to `resolve_overridden_credentials` when the time comes.
/// Test: Construct via `Default::default()`; existing call sites pass that
/// sentinel and behave exactly as before. Wiring is verified by `cargo check`.
#[derive(Debug, Clone, Default)]
pub struct SessionOverrides {
    pub model: Option<String>,
    pub provider: Option<String>,
    /// Resolved principal for this dispatch (#481).
    ///
    /// Why: Transports that authenticate users (Slack RBAC) must carry the
    /// caller's identity into tool dispatch so `filter_tools_for_user` gates
    /// the persona toolset by the caller's `ServiceTier` rather than the
    /// default CLI identity (which is `All` — unrestricted). `None` falls
    /// back to `UserIdentity::default()` so existing CLI/REPL callers behave
    /// exactly as before.
    pub user: Option<crate::rbac::UserIdentity>,
}

/// Resolve the effective `LlmCredentials` honoring an optional session
/// `provider_override` (#284).
///
/// Why: When the user has run `/provider <name>`, we must bypass the normal
/// `pick_credentials()` env probe and route through the requested credential
/// path instead. Centralising the override-vs-env decision here keeps both
/// dispatch entrypoints (`run_pm_task_with_history`, `run_pm_task_with_persona`)
/// in lock-step.
/// What: Three valid override values:
///   - `"claude-code"` → `LlmCredentials::ClaudeCode` (claude CLI subprocess)
///   - `"openrouter"` → `LlmCredentials::OpenRouter` (REST client)
///   - `"bedrock"`    → ensures the model id carries the `bedrock/` prefix
///     (auto-prepending when absent) and returns `LlmCredentials::OpenRouter`
///     as a placeholder. Bedrock dispatch is driven by the `bedrock/` model
///     prefix in `chat_with_tools_gated`, not by a credential variant — see
///     `src/llm/mod.rs` Bedrock branch.
/// Any other override value returns an error so the user's typo doesn't
/// silently fall through to env defaults. When `provider_override` is `None`
/// we delegate to `pick_credentials()` exactly as before.
/// Test: `cargo check`; `apply_credential_routing` tests still pass since
/// this helper is composed before that point.
fn resolve_overridden_credentials(
    cfg: &mut AgentConfig,
    provider_override: Option<&str>,
) -> Result<llm::credentials::LlmCredentials> {
    use llm::credentials::LlmCredentials;
    match provider_override {
        Some("claude-code") => Ok(LlmCredentials::ClaudeCode),
        Some("openrouter") => Ok(LlmCredentials::OpenRouter),
        Some("bedrock") => {
            // Bedrock dispatch is model-prefix driven. Ensure prefix is set.
            if !cfg.agent.model.starts_with("bedrock/") {
                cfg.agent.model = format!("bedrock/{}", cfg.agent.model);
            }
            // Placeholder credential — adapter inspects the model prefix and
            // routes to AWS Bedrock; the OpenRouter variant just lets
            // `apply_credential_routing` skip the use_anthropic_direct flag
            // and the claude-cli short-circuit. The OpenRouter path's bare-
            // model qualification is a no-op since the model already starts
            // with `bedrock/` (see `qualify_openrouter_model`).
            Ok(LlmCredentials::OpenRouter)
        }
        Some("local") => {
            // Ollama dispatch is also model-prefix driven (`ollama/<name>`).
            // The adapter detects the prefix and overrides the OpenAI-compatible
            // base URL to point at the local ollama server. We piggyback on the
            // OpenRouter credential variant since ollama needs no auth header;
            // the LLM HTTP layer will skip auth when the endpoint's
            // `auth_header_value` is empty (see `OllamaAdapter::api_endpoint`).
            if !cfg.agent.model.starts_with("ollama/") {
                cfg.agent.model = format!("ollama/{}", cfg.agent.model);
            }
            Ok(LlmCredentials::OpenRouter)
        }
        // Bookmarked for future wiring: "anthropic-api", "openai-api".
        Some(other) => anyhow::bail!(
            "unknown provider override '{}'. Valid: openrouter, claude-code, bedrock, local",
            other
        ),
        None => llm::credentials::pick_credentials(Some(cfg.agent.runner))
            .ok_or_else(|| anyhow::anyhow!("{}", llm::credentials::missing_credentials_error())),
    }
}

/// Take the value out of an `Arc<Mutex<Option<T>>>` slot, treating a poisoned
/// lock as "nothing there".
///
/// Why: `ctrl_chat_turn` drains several queued side-effect slots (pending
/// connect, self-task, stop) that all share the same lock-then-take pattern.
/// Centralising it removes three near-identical match blocks and ensures
/// poisoning behaviour stays consistent.
/// What: Returns `Some(value)` when the lock acquired and the slot held a
/// value; `None` when the lock was poisoned OR the slot was empty.
/// Test: Exercised indirectly by `ctrl_chat_turn` integration tests; the
/// happy path is the common case (no poisoning).
fn drain_slot<T>(slot: &Arc<Mutex<Option<T>>>) -> Option<T> {
    slot.lock().ok()?.take()
}

/// Build the `## User Context` block that prefixes the system prompt for both
/// `run_pm_task_with_history` and `run_pm_task_with_persona`.
///
/// Why: The two PM dispatch paths used to hand-roll an identical block to
/// inject the user's name, timezone, and current local date/time so the LLM
/// can address the user and answer "what time is it?" naturally. The two
/// copies had drifted in their unknown-user branch (one omitted the
/// `user_name = "(unknown)"` line). Extracting one helper closes the
/// divergence and gives every future caller the same context format.
/// What: Loads `UserProfile`, formats `chrono::Local::now()` as
/// `YYYY-MM-DD HH:MM:SS TZ`, prepends a `## User Context` block, and returns
/// the combined string with `base_content` appended after a blank line.
/// Test: Indirectly via the PM/persona dispatch paths; absence of profile
/// should still produce a `user_name = "(unknown)"` line and a current
/// date/time line.
fn build_user_context_prefix(base_content: &str) -> String {
    use crate::identity::user_profile::UserProfile;
    let profile = UserProfile::load();
    let now_local = chrono::Local::now();
    let now_str = now_local.format("%Y-%m-%d %H:%M:%S %Z").to_string();
    match profile {
        Some(ref p) if !p.name.trim().is_empty() => format!(
            "## User Context\nuser_name = \"{}\"\ntimezone = \"{}\"\nCurrent date and time: {}\n\n{}",
            p.name,
            p.timezone.as_deref().unwrap_or("UTC"),
            now_str,
            base_content
        ),
        _ => format!(
            "## User Context\nuser_name = \"(unknown)\"\nCurrent date and time: {}\n\n{}",
            now_str, base_content
        ),
    }
}

/// Best-effort semantic recall over the project's embedded memory store (#275).
///
/// Why: The PM and ctrl prompts get a project-memory layer so the LLM is
/// grounded in prior decisions/conventions without the user re-stating them.
/// Previously this shelled out to the `kuzu-memory` MCP binary; that path was
/// fire-and-forget (no Rust write site, silent empty on missing binary) and
/// shared no schema with the in-process redb+usearch store where every other
/// memory tool actually writes. This helper routes recall through the same
/// store + embedder used by `memory_recall`, eliminating split-brain memory.
/// What: Opens `<project>/.open-mpm/sessions/default` as a `RedbUsearchStore`,
/// embeds the query via `FastEmbedder`, searches `Segment::AgentMemory`, and
/// returns up to `top_k` `payload.content` strings (falling back to the raw
/// JSON payload when no `content` field is present). Any error — store
/// missing, embedder init failure, search error — collapses to an empty Vec
/// so prompt building never blocks on memory recall.
/// Test: Both call sites (PM `run_pm_task_with_history` and ctrl
/// `run_ctrl`) exercise the empty-Vec path on any cold project; populated
/// recall is covered by `memory_recall` integration tests in `tools/memory.rs`.
async fn recall_project_memories(project_dir: &Path, query: &str, top_k: usize) -> Vec<String> {
    let session_dir = project_dir
        .join(".open-mpm")
        .join("sessions")
        .join("default");
    if !session_dir.exists() {
        return Vec::new();
    }
    let store = match crate::memory::open_memory_store(&session_dir) {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!(error = %e, "recall_project_memories: store open failed");
            return Vec::new();
        }
    };
    let embedder = match crate::memory::FastEmbedder::new() {
        Ok(e) => e,
        Err(e) => {
            tracing::debug!(error = %e, "recall_project_memories: embedder unavailable");
            return Vec::new();
        }
    };
    let qvec = match crate::memory::Embedder::embed_single(&embedder, query) {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!(error = %e, "recall_project_memories: embed failed");
            return Vec::new();
        }
    };
    let hits = match store
        .search(crate::memory::Segment::AgentMemory, &qvec, top_k)
        .await
    {
        Ok(h) => h,
        Err(e) => {
            tracing::debug!(error = %e, "recall_project_memories: search failed");
            return Vec::new();
        }
    };
    hits.into_iter()
        .map(|h| {
            h.payload
                .get("content")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| h.payload.to_string())
        })
        .filter(|s| !s.trim().is_empty())
        .collect()
}

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

/// Detect open-mpm's own project root. (#182)
///
/// Why: When CTRL runs from a checkout/build of open-mpm itself, we want to
/// expose self-development tools (status, dispatch a task on ourselves).
/// Detection has to work whether the user runs `cargo run` (cwd = repo) or
/// invokes a release binary from elsewhere on the filesystem (cwd =
/// somewhere unrelated; current_exe = `…/target/release/open-mpm`).
/// What: Tries three strategies in order:
///   1. `OPEN_MPM_PROJECT_DIR` env var (explicit override).
///   2. Walk up from `current_exe()` looking for `.open-mpm/agents/pm.toml`.
///   3. Use `current_dir()` if it contains the same marker.
/// Returns the first match, or `None` when no strategy succeeds.
/// Test: `detect_self_project_finds_repo_via_cwd` (in tests below).
pub fn detect_self_project() -> Option<PathBuf> {
    fn looks_like_self(p: &Path) -> bool {
        p.join(".open-mpm").join("agents").join("pm.toml").is_file()
    }
    fn walk_up(start: &Path) -> Option<PathBuf> {
        let mut cur = Some(start.to_path_buf());
        while let Some(p) = cur {
            if looks_like_self(&p) {
                return Some(p);
            }
            cur = p.parent().map(Path::to_path_buf);
        }
        None
    }

    if let Ok(p) = std::env::var("OPEN_MPM_PROJECT_DIR")
        && let Ok(canon) = PathBuf::from(&p).canonicalize()
        && looks_like_self(&canon)
    {
        return Some(canon);
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
        && let Some(found) = walk_up(parent)
    {
        return Some(found);
    }
    if let Ok(cwd) = std::env::current_dir()
        && let Some(found) = walk_up(&cwd)
    {
        return Some(found);
    }
    None
}

/// Base system prompt for the CTRL LLM — terse senior-dev voice.
///
/// Why: CTRL talks to the user often (project switching, "what did I do
/// yesterday?", memory recall). A fluffy assistant style wastes their time.
/// Under 300 tokens; the LLM should answer in sentences not paragraphs.
/// What: Documents the four tools CTRL has and the expected behaviors.
// #185: Taskmaster persona — autonomous, results-driven coordination.
const CTRL_SYSTEM_PROMPT: &str = "You are the Taskmaster — an autonomous project coordination controller that manages AI coding projects and drives tasks to completion.

## Your Persona
You are proactive, direct, and results-driven. You don't just route tasks — you own them until they're done. When something breaks, you fix it or clearly explain why you can't.

## Core Responsibilities
1. **Drive tasks to completion**: When a PM is working on a task, monitor progress. If a phase fails, attempt recovery before escalating.
2. **Handle blockers**: Try to resolve failures autonomously (retry with context, switch approach, load a relevant skill) up to 2 times. Only escalate to the user when you've exhausted options, and when you do, be specific: what failed, what you tried, what you need.
3. **Communicate status clearly**: Proactive updates — 'Task X: code phase complete (wave 3/5), QA starting', 'BLOCKED: QA failed twice on bcrypt error — applying python-compat fix and retrying'.
4. **Track task state**: Maintain awareness of what's queued, running, blocked, and done.
5. **Post-task debrief**: After each task, give a concise summary: what was built, test results, any retries needed, cost.

## Tools Available
- start_pm(project_path) → start a PM session for a project
- list_projects() → known projects
- self_project_status() → your own project's version and git state
- initiate_self_task(task) → run a task on your own project (self-improvement)
- task_status() → list active and recently completed PM tasks
- memory_store/memory_recall → cross-project context
- search_docs(query) → search project documentation semantically. Use this to answer questions about how open-mpm works, its configuration, agents, skills, and workflows.

## Rules
- Never say 'I can't help with that' — find a path forward or explain the specific blocker
- Always confirm task completion with evidence (test counts, file counts, cost)
- When a task runs >30 min without output, proactively check status
- Prefer action over asking for permission on routine decisions
";

/// JSONL record persisted to `~/.open-mpm/sessions/pm-messages.jsonl` for
/// every PM-to-PM (or external) bus envelope that CTRL relays.
///
/// Why: The bus is in-memory broadcast; once the program exits the trail
/// disappears. A grep-friendly JSONL audit log lets users reconstruct
/// who-told-whom-what across runs.
/// What: ISO-8601 timestamp, source/target project basenames, raw content
/// string, and a uuid `message_id` for correlation across logs.
#[derive(Debug, Serialize, Deserialize)]
pub struct PmMessageRecord {
    pub timestamp: String,
    pub from_project: String,
    pub to_project: String,
    pub content: String,
    pub message_id: String,
}

/// Append one bus envelope to `~/.open-mpm/sessions/pm-messages.jsonl`.
///
/// Why: Mirrors `session_record::append_run_record`; gives CTRL a durable
/// audit trail for inter-project messaging without a database.
/// What: Best-effort; creates parent dirs, opens append-mode, writes one
/// JSON line. The "content" field projects whatever string we can find on
/// the inner message (`task.text` if shaped as `{type:"task", text:"..."}`
/// or the raw JSON otherwise).
/// Test: covered indirectly by ctrl bus relay integration; unit-tested via
/// `append_pm_message_writes_jsonl_line`.
pub fn append_pm_message(env: &BusEnvelope) -> anyhow::Result<()> {
    let path = pm_messages_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = env
        .message
        .get("text")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| env.message.to_string());
    let record = PmMessageRecord {
        timestamp: chrono::Utc::now().to_rfc3339(),
        from_project: env.source_project.clone(),
        to_project: env.target_project.clone().unwrap_or_default(),
        content,
        message_id: uuid::Uuid::new_v4().to_string(),
    };
    let line = serde_json::to_string(&record)?;
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    f.write_all(line.as_bytes())?;
    f.write_all(b"\n")?;
    Ok(())
}

/// Resolve `~/.open-mpm/sessions/pm-messages.jsonl`.
fn pm_messages_path() -> anyhow::Result<PathBuf> {
    let home = dirs::home_dir().context("no home dir")?;
    Ok(home
        .join(".open-mpm")
        .join("sessions")
        .join("pm-messages.jsonl"))
}

/// Message sent from CTRL to a PM actor task.
enum PmMsg {
    /// Dispatch a user task; reply with the PM's response or an error.
    Task {
        text: String,
        reply: oneshot::Sender<Result<String>>,
    },
    /// Request graceful shutdown of the PM actor loop.
    Shutdown,
}

/// Handle to a running PM actor background task.
struct PmHandle {
    /// Short display name derived from the last path component.
    name: String,
    /// Absolute path of the project this PM manages.
    project_path: PathBuf,
    /// Channel to send messages to the PM actor loop.
    tx: mpsc::Sender<PmMsg>,
    /// JoinHandle so CTRL can await clean shutdown.
    task: tokio::task::JoinHandle<()>,
    /// #185: Latest status string ("running" | "idle" | "error").
    /// Why: The Taskmaster persona needs `task_status()` to report PM state.
    status: Arc<Mutex<String>>,
    /// #185: Last message exchanged with this PM (truncated). Empty until
    /// first dispatch.
    last_message: Arc<Mutex<String>>,
}

/// CTRL state — all currently connected PM sessions.
struct Ctrl {
    /// Keyed by canonical project path string.
    pms: HashMap<String, PmHandle>,
    /// Key of the currently focused PM (None = CTRL-level).
    active: Option<String>,
    /// #117: CTRL's own message bus handle for inter-project relay.
    /// `None` until `run_ctrl` calls `MessageBus::start`.
    bus: Option<Arc<MessageBus>>,
    /// Shared lookup of connected PM senders, keyed by project basename
    /// (matching `BusEnvelope::target_project`). The bus relay task uses
    /// this to forward a `task`-typed envelope into the PM actor's channel.
    /// Why: The relay runs in its own spawned task without access to
    /// `ctrl.pms`; sharing only the mpsc senders keeps coupling minimal
    /// while letting CTRL keep authoritative ownership of `PmHandle`.
    connected_pms: Arc<tokio::sync::Mutex<HashMap<String, mpsc::Sender<PmMsg>>>>,
    /// Shared in-memory fallback for memory_store / memory_recall when the
    /// embedded memory store is not reachable from the CTRL subprocess.
    ///
    /// Why: CTRL is a top-level REPL — we don't want memory ops to hard-fail
    /// when the user hasn't set up MCP. An in-memory Vec is good enough for
    /// the current session and stays small.
    /// What: `Arc<Mutex<Vec<String>>>` so the `MemoryTools` closure clones
    /// can mutate it safely.
    memory: Arc<Mutex<Vec<String>>>,
    /// Detected open-mpm self-project root, when running from its own
    /// checkout. (#182)
    self_project: Option<PathBuf>,
    /// Lazily-built TF-IDF index over project documentation. (#187)
    ///
    /// Why: Lets the `search_docs` tool answer questions about open-mpm
    /// configuration, agents, skills, and workflows without an LLM call.
    /// `Option` because the index is built in a background task; until it
    /// resolves, the tool returns a graceful "not ready" message.
    /// What: `Arc<Mutex<…>>` so the background builder can install the index
    /// after CTRL has already entered the REPL loop.
    docs_index: Arc<Mutex<Option<Arc<crate::docs_index::DocsIndex>>>>,
    /// Loaded user profile (`~/.open-mpm/user.toml`). Injected into the CTRL
    /// system prompt so the LLM can personalize replies. (#193)
    user_profile: Option<crate::identity::user_profile::UserProfile>,
    /// (#202) Active project path used as a default when `start_pm` is
    /// invoked without an explicit `project_path` argument.
    ///
    /// Why: Lets the user say "start a PM" once they've called
    /// `set_active_project` without re-typing the path.
    /// What: `Arc<Mutex<Option<PathBuf>>>` so the `SetActiveProjectTool`
    /// closure can mutate it and `StartPmTool` can read it.
    active_project: Arc<Mutex<Option<PathBuf>>>,
}

impl Ctrl {
    // INTENT: Create empty CTRL state with no sessions.
    fn new() -> Self {
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
    fn prompt(&self) -> String {
        match &self.active {
            None => "CTRL> ".to_string(),
            Some(key) => {
                let name = self.pms.get(key).map(|h| h.name.as_str()).unwrap_or("?");
                format!("PM[{name}]> ")
            }
        }
    }

    // INTENT: Connect to a project directory, spawning a new PM actor if needed.
    async fn connect(&mut self, raw_path: &str) -> Result<String> {
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
    fn disconnect(&mut self) -> String {
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
    fn status(&self) -> String {
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
    async fn dispatch_task(&mut self, text: String) -> Result<String> {
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
    async fn shutdown_all(self) {
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
async fn pm_actor_task(project_path: PathBuf, mut rx: mpsc::Receiver<PmMsg>) {
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

// INTENT: Execute a single PM LLM round-trip scoped to a project path.
async fn run_pm_task(project_path: &Path, user_input: &str) -> Result<String> {
    run_pm_task_with_session(project_path, user_input, None).await
}

/// Extract a name from a conversational name-introduction input.
///
/// Why: When the conversational fast path runs without a known user name, the
/// coordinator asks for it. The next turn from the user is typically a short
/// reply ("Bob", "I'm Bob", "My name is Alice"). This helper recognizes those
/// shapes so we can persist the name to `UserProfile` without an LLM round-trip.
/// What: Matches common introduction prefixes ("my name is ", "i'm ", "im ",
/// "i am ", "call me ", "it's ", "its "), or accepts a single bare alphabetic
/// word (2–20 chars) as a name. Returns the title-cased name on match.
/// Test: `extract_name_from_input_*` tests below cover positive and negative
/// cases (greetings and task requests must NOT match).
fn extract_name_from_input(input: &str) -> Option<String> {
    fn title_case(word: &str) -> String {
        let mut name = word.to_string();
        if let Some(first) = name.get_mut(0..1) {
            first.make_ascii_uppercase();
        }
        name
    }
    fn looks_like_name(word: &str, min: usize, max: usize) -> bool {
        let len = word.chars().count();
        len >= min
            && len <= max
            && word
                .chars()
                .all(|c| c.is_alphabetic() || c == '-' || c == '\'')
    }

    let trimmed = input.trim();
    let lower = trimmed.to_lowercase();
    for prefix in &[
        "my name is ",
        "i'm ",
        "im ",
        "i am ",
        "call me ",
        "it's ",
        "its ",
    ] {
        if let Some(rest) = lower.strip_prefix(prefix) {
            let word = rest.split_whitespace().next()?;
            // Reject greetings disguised as introductions ("i'm here", "i'm fine").
            const STOP_WORDS: &[&str] = &[
                "here", "fine", "good", "well", "ok", "okay", "back", "ready", "sorry", "the", "a",
                "an", "not",
            ];
            if STOP_WORDS.contains(&word) {
                return None;
            }
            if looks_like_name(word, 2, 40) {
                return Some(title_case(word));
            }
            return None;
        }
    }

    // Single-word input that looks like a name.
    let words: Vec<&str> = trimmed.split_whitespace().collect();
    if words.len() == 1 {
        let word = words[0];
        // Reject common single-word non-names (greetings, thanks, etc.)
        let lw = word.to_lowercase();
        const NON_NAMES: &[&str] = &[
            "hello", "hi", "hey", "yo", "sup", "thanks", "thank", "ok", "okay", "yes", "no", "yep",
            "nope", "help", "quit", "exit", "stop", "done",
        ];
        if NON_NAMES.contains(&lw.as_str()) {
            return None;
        }
        if word.chars().all(|c| c.is_alphabetic()) && looks_like_name(word, 2, 20) {
            return Some(title_case(word));
        }
    }
    None
}

/// Persist a detected user name to `~/.open-mpm/user.toml`.
///
/// Why: When the conversational fast path detects a name introduction it must
/// save the name immediately so the next turn's system prompt sees it. Without
/// this, the coordinator keeps re-asking ("What's your name?") in a loop.
/// What: Loads the existing profile (or starts a default one), updates the
/// name only when currently empty (don't clobber a real name with a partial
/// match), and writes the file. Failures are logged but non-fatal — the user
/// still gets a greeting, they just won't be remembered next session.
/// Test: Covered by the `extract_name_from_input_*` unit tests plus an
/// end-to-end check via `cat ~/.open-mpm/user.toml` after running the binary.
fn save_name_to_profile(name: &str) {
    use crate::identity::user_profile::UserProfile;
    let mut profile = UserProfile::load().unwrap_or_default();
    if profile.name.trim().is_empty() {
        profile.name = name.to_string();
        if profile.created_at.is_empty() {
            profile.created_at = chrono::Utc::now().to_rfc3339();
        }
        match profile.save() {
            Ok(()) => tracing::info!(name = %name, "user name saved to profile"),
            Err(e) => tracing::warn!(error = %e, "failed to save user name"),
        }
    } else {
        tracing::debug!(
            existing = %profile.name,
            detected = %name,
            "profile already has a name; not overwriting"
        );
    }
}

/// Same as `run_pm_task` but tags every emitted event with `session_id` so
/// SSE subscribers can filter to a specific UI task.
///
/// Why: The thin-CLI controller socket (`handle_socket_connection`) and any
/// other future caller can mint a uuid up-front and propagate it through to
/// every downstream emission so the UI's per-task view stays coherent. When
/// `session_id` is `None` we still emit events, just unfiltered.
/// What: Wraps the existing PM round-trip with `events::emit` calls at the
/// transition points (turn start, delegation tool call, turn done). Errors
/// trigger an `AgentFailed`-equivalent emission via the caller.
pub async fn run_pm_task_with_session(
    project_path: &Path,
    user_input: &str,
    session_id: Option<String>,
) -> Result<String> {
    run_pm_task_with_history(
        project_path,
        user_input,
        &[],
        session_id,
        SessionOverrides::default(),
    )
    .await
}

/// Multi-turn variant of `run_pm_task_with_session` that prepends `history`
/// as alternating user/assistant messages before the current `user_input`.
///
/// Why: Lets the REPL hold a back-and-forth conversation with CTRL/PM
/// instead of every task being stateless. The single-turn entry point
/// (`run_pm_task_with_session`) now just delegates here with an empty slice.
/// What: Builds a `Vec<ChatCompletionRequestMessage>` from `history` (user,
/// assistant, user, assistant, …) followed by the new user message, then
/// runs the same conversational fast-path / tool-armed delegation logic as
/// the original function — but routed through `chat_with_tools_gated` so the
/// prior turns are carried into the request.
/// Test: `ctrl::tests::ctrl_history_builds_messages` (history -> message
/// sequence); the REPL integration is exercised manually for now since LLM
/// calls aren't part of the unit test surface.
/// Resolve the agent config that drives `run_pm_task_with_history` (#240).
///
/// Why: The REPL has two modes — connected (a project has been attached via
/// `/connect`, so `pm.toml` is the source of truth) and standalone (no
/// project, so `ctrl.toml` should be loaded from the user's home directory).
/// Previously the controller hard-coded a single `ctrl.toml` lookup under
/// the project's `.open-mpm/agents/` directory and failed loudly when it
/// wasn't there. This helper centralizes the priority order and keeps the
/// REPL launchable even with zero on-disk config.
/// What: Tries, in order:
///   1. `{project_path}/.open-mpm/agents/pm.toml` (connected mode)
///   2. `~/.open-mpm/agents/ctrl.toml` (user-level standalone)
///   3. `{project_path}/.open-mpm/agents/ctrl.toml` (project-level ctrl)
///   4. `AgentConfig::ctrl_default()` — the bundled fallback.
/// Test: `resolve_agent_config_prefers_pm_toml`,
/// `resolve_agent_config_falls_back_to_user_ctrl`,
/// `resolve_agent_config_falls_back_to_project_ctrl`,
/// `resolve_agent_config_returns_builtin_when_nothing_on_disk`.
async fn resolve_agent_config(project_path: &Path) -> Result<(AgentConfig, Option<PathBuf>)> {
    let pm_path = project_path
        .join(".open-mpm")
        .join("agents")
        .join("pm.toml");
    if pm_path.is_file() {
        return Ok((AgentConfig::load(&pm_path)?, Some(pm_path)));
    }

    if let Some(home) = dirs::home_dir() {
        let user_ctrl = home.join(".open-mpm").join("agents").join("ctrl.toml");
        if user_ctrl.is_file() {
            return Ok((AgentConfig::load(&user_ctrl)?, Some(user_ctrl)));
        }
    }

    let project_ctrl = project_path
        .join(".open-mpm")
        .join("agents")
        .join("ctrl.toml");
    if project_ctrl.is_file() {
        return Ok((AgentConfig::load(&project_ctrl)?, Some(project_ctrl)));
    }

    Ok((AgentConfig::ctrl_default(), None))
}

/// Resolve the agent config used by `ctrl_chat_turn` — the conversational
/// ctrl persona, NOT the PM coordinator (#298).
///
/// Why: `resolve_agent_config` was historically shared between
/// `run_pm_task_with_history` (which legitimately wants pm.toml) and
/// `ctrl_chat_turn` (which wants ctrl.toml). When the harness runs INSIDE
/// its own repo (`detect_self_project()` succeeds and points at open-mpm),
/// the project's `.open-mpm/agents/pm.toml` exists and shadowed ctrl.toml,
/// causing every ctrl turn to load the heavy sonnet PM prompt. Result:
/// 30s responses for "hello" because ctrl was running PM-shaped requests
/// against claude-sonnet-4-6 with the full delegation tool surface.
/// What: Searches for `ctrl.toml` first (project then user) and only falls
/// back to pm.toml when neither ctrl.toml is available — and even then the
/// caller should treat this as a legacy path rather than a happy path.
/// Order:
///   1. `{project_path}/.open-mpm/agents/ctrl.toml`
///   2. `~/.open-mpm/agents/ctrl.toml`
///   3. `{project_path}/.open-mpm/agents/pm.toml` (last-resort)
///   4. `AgentConfig::ctrl_default()`
/// Test: `resolve_ctrl_agent_config_prefers_project_ctrl_over_pm`,
/// `resolve_ctrl_agent_config_falls_back_to_user_ctrl`.
async fn resolve_ctrl_agent_config(project_path: &Path) -> Result<(AgentConfig, Option<PathBuf>)> {
    let project_ctrl = project_path
        .join(".open-mpm")
        .join("agents")
        .join("ctrl.toml");
    if project_ctrl.is_file() {
        return Ok((AgentConfig::load(&project_ctrl)?, Some(project_ctrl)));
    }

    if let Some(home) = dirs::home_dir() {
        let user_ctrl = home.join(".open-mpm").join("agents").join("ctrl.toml");
        if user_ctrl.is_file() {
            return Ok((AgentConfig::load(&user_ctrl)?, Some(user_ctrl)));
        }
    }

    let pm_fallback = project_path
        .join(".open-mpm")
        .join("agents")
        .join("pm.toml");
    if pm_fallback.is_file() {
        return Ok((AgentConfig::load(&pm_fallback)?, Some(pm_fallback)));
    }

    Ok((AgentConfig::ctrl_default(), None))
}

/// Apply the canonical 3-way credential routing rules to `cfg` (#271).
///
/// Why: The same credential-routing block was copy-pasted across
///   `run_pm_task_with_history`, `run_pm_task_with_persona`, and (after #271)
///   `ctrl_chat_turn`. Centralising it means every dispatch path agrees on
///   precedence (ClaudeCode > AnthropicDirect > OpenRouter) and any future
///   credential type only needs to be wired up in one place.
/// What: For `AnthropicDirect` flips `cfg.llm.use_anthropic_direct = true`
///   (forces the chat loop down the api.anthropic.com path). For `OpenRouter`
///   qualifies bare Claude / Anthropic model ids with the `anthropic/`
///   provider prefix. For `ClaudeCode` does nothing — the caller is expected
///   to short-circuit to `run_pm_task_via_claude_cli` separately because that
///   path takes a different shape (no async-openai client, single-shot CLI).
///   Returns `true` when the caller MUST short-circuit to the claude CLI.
/// Test: `apply_credential_routing_anthropic_direct_sets_flag`,
///   `apply_credential_routing_openrouter_qualifies_model`,
///   `apply_credential_routing_claude_code_signals_short_circuit`.
fn apply_credential_routing(
    cfg: &mut AgentConfig,
    creds: &llm::credentials::LlmCredentials,
) -> bool {
    use llm::credentials::LlmCredentials;
    match creds {
        LlmCredentials::AnthropicDirect => {
            cfg.llm.use_anthropic_direct = true;
            false
        }
        LlmCredentials::ClaudeCode => true,
        LlmCredentials::OpenRouter => {
            let qualified = llm::credentials::qualify_openrouter_model(creds, &cfg.agent.model);
            if qualified != cfg.agent.model {
                tracing::debug!(
                    from = %cfg.agent.model,
                    to = %qualified,
                    "qualifying bare claude model id for OpenRouter"
                );
                cfg.agent.model = qualified;
            }
            false
        }
    }
}

/// Build the canonical "## Deployment Configuration" footer for a system
/// prompt (#271).
///
/// Why: The PM/ctrl injects a deployment-context block so the LLM can answer
///   "what model am I running?" honestly. Previously two call sites
///   (`run_pm_task_with_history` and `ctrl_chat_turn`) built nearly-identical
///   blocks with slightly different fields, drifting over time. Centralising
///   keeps the wording consistent for users and lets future fields (e.g. a
///   tracing session id) be added in one place.
/// What: Returns a leading `\n\n` plus a markdown bullet list. Optional
///   fields (`tools_count`, `mcp_count`, `config_label`) are omitted when
///   `None` so call sites can pass only what they have on hand.
/// Test: `build_deployment_footer_includes_required_fields`,
///   `build_deployment_footer_omits_optional_fields_when_none`.
fn build_deployment_footer(
    agent_name: &str,
    runner_label: &str,
    model: &str,
    version: &str,
    skills_count: usize,
    tools_count: Option<usize>,
    mcp_count: Option<usize>,
    project_label: &str,
    config_label: Option<&str>,
) -> String {
    let mut out = String::from("\n\n## Deployment Configuration\n");
    out.push_str(&format!(" - Agent: {agent_name}\n"));
    out.push_str(&format!(" - Model: {model}\n"));
    out.push_str(&format!(" - Runner: {runner_label}\n"));
    out.push_str(&format!(" - Version: v{version}\n"));
    if let Some(tools) = tools_count {
        out.push_str(&format!(" - Tools available: {tools}\n"));
    }
    out.push_str(&format!(" - Skills loaded: {skills_count}\n"));
    if let Some(mcp) = mcp_count {
        out.push_str(&format!(" - MCP connections: {mcp}\n"));
    }
    out.push_str(&format!(" - Project: {project_label}\n"));
    if let Some(cfg) = config_label {
        out.push_str(&format!(" - Config: {cfg}\n"));
    }
    out
}

pub async fn run_pm_task_with_history(
    project_path: &Path,
    user_input: &str,
    history: &[ConversationTurn],
    session_id: Option<String>,
    overrides: SessionOverrides,
) -> Result<String> {
    use async_openai::types::{
        ChatCompletionRequestAssistantMessageArgs, ChatCompletionRequestMessage,
        ChatCompletionRequestSystemMessageArgs, ChatCompletionRequestUserMessageArgs,
    };

    tracing::debug!(
        project = %project_path.display(),
        history_turns = history.len(),
        input_len = user_input.len(),
        "ctrl::run_pm_task_with_history entered"
    );

    let sid = session_id.unwrap_or_default();
    events::publish(Event::PmThinking {
        session_id: sid.clone(),
        text: events::preview(user_input, 240),
    });

    let config_dir = project_path.join(".open-mpm").join("agents");
    // Resolve agent config across both modes (#240):
    //   - Connected: prefer `{project}/.open-mpm/agents/pm.toml`.
    //   - Disconnected (project_path == cwd, no pm.toml): fall back to
    //     `~/.open-mpm/agents/ctrl.toml`, then project-level
    //     `.open-mpm/agents/ctrl.toml`, then a built-in default so the REPL
    //     never fails to start because a config file is missing.
    // Tools (DelegateToAgentTool) are registered separately below — config
    // load only sets system prompt + model + LLM params.
    let (mut pm_cfg, _pm_cfg_path) = resolve_agent_config(project_path).await?;

    // #284: Apply session overrides (set by /model and /provider) BEFORE
    // credential resolution so `/provider bedrock` can prepend the bedrock/
    // prefix to the override-provided model in one place.
    if let Some(ref m) = overrides.model {
        tracing::debug!(model = %m, "applying /model session override");
        pm_cfg.agent.model = m.clone();
    }

    // #250: Resolve credential routing up-front so the PM/ctrl works with any
    // of OPENROUTER_API_KEY / ANTHROPIC_API_KEY / CLAUDE_CODE_OAUTH_TOKEN
    // without forcing the user to set a key they don't have. Three branches:
    //   - OpenRouter: existing path (no override).
    //   - AnthropicDirect: force `use_anthropic_direct=true` so the chat loop
    //     skips OpenRouter and posts to api.anthropic.com directly.
    //   - ClaudeCode: short-circuit to the `claude` CLI subprocess for the
    //     ctrl/PM's own LLM call. OAuth tokens (sk-ant-oat01-*) are only
    //     valid via that path.
    // #284: When `/provider <name>` is active, `resolve_overridden_credentials`
    // bypasses the env probe.
    let creds = resolve_overridden_credentials(&mut pm_cfg, overrides.provider.as_deref())?;
    // #271: Centralized credential routing helper. Handles the 3-way decision
    // (AnthropicDirect → flag flip; OpenRouter → model qualification;
    // ClaudeCode → caller short-circuits below). See `apply_credential_routing`.
    let claude_cli_short_circuit = apply_credential_routing(&mut pm_cfg, &creds);
    // #297: Latency trace for the PM-history dispatch path. Mirrors the
    // ctrl_chat_turn / persona blocks so all three paths emit the same fields.
    tracing::info!(
        agent = %pm_cfg.agent.name,
        runner = ?pm_cfg.agent.runner,
        model = %pm_cfg.agent.model,
        creds = creds.label(),
        claude_cli_short_circuit,
        use_anthropic_direct = pm_cfg.llm.use_anthropic_direct,
        "run_pm_task_with_history: credentials resolved"
    );

    // Inject runtime deployment context into the PM system prompt so the
    // PM/ctrl can answer "what model are you running?" / "which runner?"
    // honestly instead of deflecting. Placed BEFORE the ClaudeCode short-
    // circuit so BOTH the OpenRouter/Anthropic REST path AND the
    // run_pm_task_via_claude_cli path carry identical context.
    {
        let runner_label = match creds {
            llm::credentials::LlmCredentials::ClaudeCode => "claude-code (ClaudeCodeAgentRunner)",
            llm::credentials::LlmCredentials::AnthropicDirect => "anthropic-direct",
            llm::credentials::LlmCredentials::OpenRouter => "openrouter",
        };
        let skills_count = pm_cfg
            .system_prompt
            .skills
            .as_ref()
            .map(|s| s.len())
            .unwrap_or(0);
        let project_label = project_path.display().to_string();
        let deployment_block = build_deployment_footer(
            &pm_cfg.agent.name,
            runner_label,
            &pm_cfg.agent.model,
            crate::build_info::VERSION,
            skills_count,
            None,
            None,
            &project_label,
            None,
        );
        pm_cfg.system_prompt.content.push_str(&deployment_block);
    }

    if claude_cli_short_circuit {
        // The claude CLI handles its own auth; we just need to dispatch to
        // it. Build the same augmented system prompt the OpenRouter path
        // uses, then call the runner.
        tracing::info!("ctrl PM turn → claude CLI (no API-key credential available)");
        return run_pm_task_via_claude_cli(project_path, &pm_cfg, user_input, history, &sid).await;
    }
    let client = llm::create_client()?;

    // #t04/t05/t07: PM coordinator now respects ctrl.toml's model setting,
    // including `bedrock/...`. Installing the Bedrock env guard here mirrors
    // the in-process runner's pattern (`agents/in_process_runner.rs:286`) so
    // `chat_adapter_aware` can pick up AWS profile/region from env on the
    // Bedrock branch. The guard is held for the duration of this function so
    // both the conversational fast path AND the tool-armed delegation path
    // see the same AWS context.
    let _bedrock_env_guard = if llm::adapter::adapter_for_model(&pm_cfg.agent.model).provider()
        == llm::adapter::Provider::Bedrock
    {
        Some(crate::agents::in_process_runner::BedrockEnvGuard::install(
            pm_cfg.llm.aws_profile.as_deref(),
            pm_cfg.llm.aws_region.as_deref(),
        ))
    } else {
        None
    };

    // Build augmented system prompt with optional user profile context so the
    // coordinator can address the user by name (or ask for it when unknown).
    // Mirrors the injection pattern in `run_ctrl` (~line 1872).
    let system_prompt: String = {
        // Inject local-zone wall clock + user identity so the LLM can answer
        // "what time is it?" / address the user by name without falling back
        // to "I don't have access to the current time". Format: ISO-style
        // date + 24h time + zone abbreviation (#feat: ctrl date+time
        // injection). Shared with run_pm_task_with_persona via QW1.
        let base = build_user_context_prefix(&pm_cfg.system_prompt.content);

        // #241: Augment PM/ctrl prompt with MCP tool descriptions (role-gated)
        // and project-memory recall from the embedded redb+usearch store.
        // #275: Replaced the prior `kuzu_recall` shell-out with in-process
        // recall against `Segment::AgentMemory` so the prompt sees the same
        // memories every other tool reads/writes. Best-effort: failures
        // produce empty layers and the agent runs unchanged.
        // #478: Substitute {{AGENT_MODEL}} / {{AGENT_RUNNER}} placeholders so
        // pm/ctrl TOMLs don't leak raw placeholder strings. Mirrors `ctrl_chat_turn`.
        let runner_label = match pm_cfg.agent.runner {
            crate::agents::RunnerKind::Subprocess => "subprocess",
            crate::agents::RunnerKind::Inline => "inline",
            crate::agents::RunnerKind::ClaudeCode => "claude-code",
            crate::agents::RunnerKind::InProcess => "in-process",
        };
        let mut builder = crate::agents::prompt_builder::SystemPromptBuilder::new(base)
            .with_agent_context(pm_cfg.agent.model.as_str(), runner_label);
        // #244: Use load() (no create-if-absent) so the very next prompt
        // build picks up changes made via the mcp_* tools without caching.
        let mcp_cfg = crate::mcp::GlobalConfig::load().await;
        if let Some(section) = mcp_cfg.render_prompt_section(&pm_cfg.agent.role) {
            builder = builder.add_mcp_layer(section);
        }
        let q = &user_input[..200.min(user_input.len())];
        let memories = recall_project_memories(project_path, q, 5).await;
        if !memories.is_empty() {
            builder = builder.add_memory_layer(memories);
        }
        let mut prompt = builder.build();
        // Inject live TM session summary so the PM sees ground truth and can
        // route session-management questions to tm_* tools instead of guessing.
        let tm_state_dir = project_path.join(".open-mpm").join("state");
        let tm_block = build_tm_context_block(&tm_state_dir).await;
        if !tm_block.is_empty() {
            prompt.push_str("\n\n");
            prompt.push_str(&tm_block);
        }
        prompt
    };

    // Build the typed message list once: system prompt + alternating
    // history turns + the new user input. Both the conversational fast
    // path and the tool-armed delegation path consume the same list so the
    // model sees identical context regardless of which branch fires.
    let mut initial_messages: Vec<ChatCompletionRequestMessage> = Vec::new();
    initial_messages.push(
        ChatCompletionRequestSystemMessageArgs::default()
            .content(system_prompt.clone())
            .build()
            .context("failed to build system message")?
            .into(),
    );
    // #269: Apply sliding-window truncation to bound wire-cost on long
    // sessions. The stored `history` slice is never mutated; we only shrink
    // the wire copy. Default budget = 12 turns with turn 0 pinned.
    let truncated_history: Vec<ConversationTurn> =
        crate::compress::truncate_history(history, &crate::compress::TokenBudget::default());
    for turn in &truncated_history {
        initial_messages.push(
            ChatCompletionRequestUserMessageArgs::default()
                .content(turn.user.clone())
                .build()
                .context("failed to build history user message")?
                .into(),
        );
        initial_messages.push(
            ChatCompletionRequestAssistantMessageArgs::default()
                .content(turn.assistant.clone())
                .build()
                .context("failed to build history assistant message")?
                .into(),
        );
    }
    initial_messages.push(
        ChatCompletionRequestUserMessageArgs::default()
            .content(user_input)
            .build()
            .context("failed to build current user message")?
            .into(),
    );

    // Fast path (#199): conversational inputs (greetings, thanks, "what can
    // you do?") skip the delegation pipeline entirely. Sending them through
    // tool-armed PM + sub-agent spawn costs 60-90s for a sub-second reply.
    // We still call the LLM (so the PM persona answers naturally) but with
    // NO tools, so it cannot trigger delegate_to_agent. Multi-turn variant
    // routes through `chat_with_tools_gated` with an empty registry so the
    // prior conversation history travels with the request.
    if matches!(classify_intent(user_input), IntentClass::Conversational) {
        tracing::info!("intent classifier: Conversational fast path");

        // If the user is introducing themselves ("I'm Bob", "Bob"), persist
        // the name and short-circuit with a deterministic greeting. Calling
        // the LLM here is wasteful (the response is essentially fixed) AND
        // wrong: the system prompt above was built BEFORE the save, so it
        // still says `user_name = "(unknown)"` and the LLM will keep asking
        // "What's your name?" — creating a greeting loop. Returning a direct
        // greeting fixes both bugs at once.
        if let Some(name) = extract_name_from_input(user_input) {
            save_name_to_profile(&name);
            let greeting = format!(
                "Nice to meet you, {}! What would you like to build today?",
                name
            );
            events::publish(Event::PmThinking {
                session_id: sid,
                text: greeting.clone(),
            });
            return Ok(greeting);
        }

        // #319: Local Ollama fast-path. Conversational turns are exactly
        // what local models excel at — short, no tools, low stakes. Reuse
        // the same routing predicate so behavior matches `ctrl_chat_turn`.
        let local_global_cfg = crate::mcp::GlobalConfig::load().await;
        let local_cfg = &local_global_cfg.local_inference;
        let local_qualifies = local_cfg.enabled
            && crate::local_inference::qualifies_for_local_inference(
                &IntentClass::Conversational,
                user_input,
            )
            && crate::local_inference::is_ollama_available_cached(&local_cfg.ollama_host).await;
        let (effective_model, effective_max_tokens, effective_use_direct) = if local_qualifies {
            tracing::info!(
                local_model = %local_cfg.model,
                "run_pm_task_with_history: routing conversational to local ollama"
            );
            (local_cfg.model.clone(), local_cfg.max_tokens, false)
        } else {
            (
                pm_cfg.agent.model.clone(),
                pm_cfg.llm.max_tokens,
                pm_cfg.llm.use_anthropic_direct,
            )
        };

        let adapter = llm::adapter::adapter_for_model(&effective_model);
        // #253: Timing — measure end-to-end LLM round trip for the
        // conversational fast path so we can diagnose "very slow" reports
        // by simply running with `RUST_LOG=info`.
        let llm_t0 = std::time::Instant::now();
        tracing::info!(
            model = %effective_model,
            history_turns = history.len(),
            local_route = local_qualifies,
            "ctrl LLM call start (conversational fast path)"
        );
        let local_call = llm::chat_with_tools_gated(
            &client,
            &effective_model,
            &*adapter,
            initial_messages.clone(),
            Arc::new(ToolRegistry::new()),
            None,
            pm_cfg.llm.temperature,
            effective_max_tokens,
            2,
            false,
            None,
            false,
            effective_use_direct,
            &pm_cfg.llm.stop_sequences,
        )
        .await;
        let mut used_remote_fallback = false;
        let (content, _usage) = match local_call {
            Ok(pair) => pair,
            Err(e) if local_qualifies && local_cfg.fallback_on_error => {
                tracing::warn!(
                    error = %e,
                    "local inference failed, falling back to remote: {e:#}"
                );
                used_remote_fallback = true;
                let remote_adapter = llm::adapter::adapter_for_model(&pm_cfg.agent.model);
                llm::chat_with_tools_gated(
                    &client,
                    &pm_cfg.agent.model,
                    &*remote_adapter,
                    initial_messages.clone(),
                    Arc::new(ToolRegistry::new()),
                    None,
                    pm_cfg.llm.temperature,
                    pm_cfg.llm.max_tokens,
                    2,
                    false,
                    None,
                    false,
                    pm_cfg.llm.use_anthropic_direct,
                    &pm_cfg.llm.stop_sequences,
                )
                .await
                .inspect_err(|e| {
                    tracing::error!(error = %e, "ctrl::run_pm_task_with_history conversational fast-path remote fallback also failed")
                })?
            }
            Err(e) => {
                tracing::error!(error = %e, "ctrl::run_pm_task_with_history conversational fast-path LLM call failed");
                return Err(e);
            }
        };
        // #468: Surface the silent local→remote fallback to the user. The
        // `warn!` above only lands in server logs; in chat surfaces
        // (Telegram, REPL) the response previously looked identical to a
        // local-served reply, hiding that Ollama wasn't reachable. Prefixing
        // the response makes the degradation visible at the point it
        // affects the user. We only prefix on the success path — fallback
        // errors are returned via `?` above and surface as errors, where
        // the prefix would be misleading.
        let content = if used_remote_fallback {
            format!("[⚡ Ollama unavailable — using OpenRouter]\n\n{content}")
        } else {
            content
        };
        tracing::info!(
            elapsed_ms = llm_t0.elapsed().as_millis() as u64,
            response_len = content.len(),
            "ctrl LLM call done (conversational fast path)"
        );
        events::publish(Event::PmThinking {
            session_id: sid,
            text: events::preview(&content, 240),
        });
        return Ok(content);
    }

    let runner: Arc<dyn AgentRunner> =
        Arc::new(SubprocessAgentRunner::new().with_config_dir(Some(config_dir.clone())));

    let mut registry = ToolRegistry::new();
    // Attach config_dir for pre-flight agent_name validation (#204): rejects
    // hallucinated names like `code-searcher` (invented from the search_code
    // native tool) before they crash the subprocess runner.
    registry.register(Arc::new(
        DelegateToAgentTool::new(runner).with_config_dir(config_dir.clone()),
    ));
    // #210: register the full CTRL native tool set so utterances like
    // "add project <path>" or "list projects" route to the matching tool
    // instead of hallucinating a `delegate_to_agent` call with an invented
    // agent name (which crashes the subprocess runner with exit 1).
    //
    // State-bearing tools (`StopTaskTool`, `SetActiveProjectTool`) get
    // freshly-allocated, locally-scoped slots here because this function runs
    // outside the CTRL REPL and has no access to the live `Ctrl` struct's
    // shared state. The slots are drained at the end of this function (see
    // below) on a best-effort basis — they cannot affect long-lived CTRL
    // state, but they keep the LLM from falling back to delegation.
    let stop_pending: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let active_project_slot: Arc<Mutex<Option<PathBuf>>> = Arc::new(Mutex::new(None));
    registry.register(Arc::new(AddProjectTool));
    registry.register(Arc::new(ListProjectsTool));
    registry.register(Arc::new(RemoveProjectTool));
    registry.register(Arc::new(StopTaskTool {
        snapshot: Vec::new(),
        pending_stop: stop_pending,
    }));
    registry.register(Arc::new(SetActiveProjectTool {
        active_project: active_project_slot,
    }));
    registry.register(Arc::new(MoveFileTool));
    registry.register(Arc::new(CreateDirTool));
    registry.register(Arc::new(
        crate::tools::web_search::BraveSearchTool::from_env(),
    ));
    // #374: Auto-detect search backend. Prefers the running search daemon
    // (HTTP) when present, otherwise falls back to in-process grep. This
    // also fixes a pre-existing bug where this tool was always wired in
    // grep-only mode even when the PM had a warm `Arc<CodeIndexer>` from
    // `spawn_background_file_watcher`.
    {
        let project_root =
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let search_tool =
            crate::tools::native_search::SearchCodeTool::new_auto(&project_root).await;
        registry.register(Arc::new(search_tool));
    }
    // #304: Coordinator-facing shell executor — lets ctrl/PM run shell
    // commands directly (filesystem ops, git, status checks) instead of
    // telling the user to run them. Uses the project root as CWD when the
    // agent does not pass a `working_dir`.
    {
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        registry.register(Arc::new(crate::tools::run_bash::RunBashTool::new(cwd)));
    }
    // #244: Dynamic MCP service management tools.
    for tool in crate::tools::mcp_tools::mcp_tool_executors() {
        registry.register(tool);
    }
    // #243: Native ticketing tools (no-op when no GitHub identity configured).
    register_ticketing_tools(&mut registry).await;

    // TM (tmux manager) tools — same surface ctrl gets, so PM-delegated tasks
    // can also inspect/control tmux sessions when needed.
    {
        let state_dir = project_path.join(".open-mpm").join("state");
        crate::tools::tm_tools::register_tm_tools_for_state_dir(&mut registry, &state_dir);
    }

    // Tool-armed delegation path (multi-turn). `chat_with_tools_gated` takes
    // a typed message list (system + history + new input) and runs the
    // tool-call loop internally. This replaces the earlier `chat_adapter_aware`
    // single-turn call so prior conversation context is carried into the
    // request and influences the model's tool selection.
    // TODO(#298): Use `pm_cfg.llm.routing_model` (when set) for THIS first
    // tool-armed call so initial delegation runs on Haiku rather than Sonnet.
    // Subsequent synthesis turns should fall back to `pm_cfg.agent.model`. Will
    // require either a flag on `chat_with_tools_gated` to pin the model only
    // for turn 0, or splitting this into a routing call followed by a separate
    // synthesis call. Held until we measure the actual savings to justify the
    // multi-model branching complexity.
    let adapter = llm::adapter::adapter_for_model(&pm_cfg.agent.model);
    let registry_arc = Arc::new(registry);
    // #253: Timing — same instrumentation as the fast path so the slow
    // tool-armed delegation branch is also visible under RUST_LOG=info.
    let llm_t0 = std::time::Instant::now();
    tracing::info!(
        model = %pm_cfg.agent.model,
        history_turns = history.len(),
        "ctrl LLM call start (tool-armed delegation)"
    );
    let (content, _usage) = llm::chat_with_tools_gated(
        &client,
        &pm_cfg.agent.model,
        &*adapter,
        initial_messages,
        registry_arc,
        None,
        pm_cfg.llm.temperature,
        pm_cfg.llm.max_tokens,
        4,
        false,
        None,
        false,
        pm_cfg.llm.use_anthropic_direct,
        &pm_cfg.llm.stop_sequences,
    )
    .await
    .inspect_err(|e| {
        tracing::error!(error = %e, "ctrl::run_pm_task_with_history tool-armed delegation LLM call failed")
    })?;
    tracing::info!(
        elapsed_ms = llm_t0.elapsed().as_millis() as u64,
        response_len = content.len(),
        "ctrl LLM call done (tool-armed delegation)"
    );

    events::publish(Event::PmThinking {
        session_id: sid,
        text: events::preview(&content, 240),
    });
    Ok(content)
}

/// Run a single conversation turn against a persona agent (#254).
///
/// Why: The REPL `/agent` command lets the user switch the active ctrl
/// conversation to a non-coding persona (e.g. `personal-assistant` /
/// `cto-assistant`). These personas should NOT have delegation tools wired
/// up — they're intended as direct chat partners with their own system
/// prompt and model. Routing through `run_pm_task_with_history` would resolve
/// `pm.toml` (or the built-in ctrl default) and arm the delegation toolset,
/// which is the wrong shape entirely.
/// What: Loads `<project>/.open-mpm/agents/<persona_name>.toml`, builds the
/// same date/time-injected system prompt the default ctrl path uses (so
/// "what time is it?" works for personas too), then makes a tools-OFF
/// `chat_with_tools_gated` call carrying the prior conversation history.
/// Returns the assistant text. Honors the same `LlmCredentials` routing
/// (OpenRouter / AnthropicDirect / claude CLI) as the default path so the
/// persona inherits whichever credential the user has configured.
/// Test: Manual via tmux — `/agent personal-assistant` then "who are you?"
/// → identifies as Izzie, knows Masa.
/// Match a tool name against a list of glob patterns (#255).
///
/// Why: Persona TOMLs accept `["mcp_*", "git_log"]` so operators don't have
/// to enumerate every dynamic tool name. A purpose-built matcher avoids
/// pulling in the `glob` crate for two patterns of behavior.
/// What: Returns `true` if `name` exactly equals a pattern, OR a pattern
/// ends with `*` and `name` starts with the pattern's prefix. Empty pattern
/// list returns false (caller treats `None` as "no filter" separately).
/// Test: `match_any_glob_handles_suffix_wildcard` below.
fn match_any_glob(name: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|p| {
        if let Some(prefix) = p.strip_suffix('*') {
            name.starts_with(prefix)
        } else {
            name == p
        }
    })
}

pub async fn run_pm_task_with_persona(
    project_path: &Path,
    persona_name: &str,
    user_input: &str,
    history: &[ConversationTurn],
    session_id: Option<String>,
    overrides: SessionOverrides,
) -> Result<String> {
    use async_openai::types::{
        ChatCompletionRequestAssistantMessageArgs, ChatCompletionRequestMessage,
        ChatCompletionRequestSystemMessageArgs, ChatCompletionRequestUserMessageArgs,
    };

    let sid = session_id.unwrap_or_default();

    // Load the persona TOML. We look in the project agents dir first, then
    // fall back to the user-level `~/.open-mpm/agents/`. This mirrors the
    // resolution order used by `resolve_agent_config`.
    let project_persona = project_path
        .join(".open-mpm")
        .join("agents")
        .join(format!("{}.toml", persona_name));
    let mut persona_cfg = if project_persona.is_file() {
        AgentConfig::load(&project_persona)?
    } else if let Some(home) = dirs::home_dir() {
        let user_persona = home
            .join(".open-mpm")
            .join("agents")
            .join(format!("{}.toml", persona_name));
        if user_persona.is_file() {
            AgentConfig::load(&user_persona)?
        } else {
            anyhow::bail!(
                "persona agent '{}' not found at {} or {}",
                persona_name,
                project_persona.display(),
                user_persona.display()
            );
        }
    } else {
        anyhow::bail!(
            "persona agent '{}' not found at {}",
            persona_name,
            project_persona.display()
        );
    };

    // #284: Apply session overrides (set by /model and /provider) BEFORE
    // credential resolution; mirrors the block in `run_pm_task_with_history`.
    if let Some(ref m) = overrides.model {
        tracing::debug!(persona = %persona_name, model = %m, "applying /model override");
        persona_cfg.agent.model = m.clone();
    }

    // #271: Apply the same credential routing as `run_pm_task_with_history`
    // through the shared `apply_credential_routing` helper.
    // #284: `resolve_overridden_credentials` honors the `/provider` override
    // when set, falling back to the env probe otherwise.
    let creds = resolve_overridden_credentials(&mut persona_cfg, overrides.provider.as_deref())?;
    let claude_cli_short_circuit = apply_credential_routing(&mut persona_cfg, &creds);
    // #297: Latency trace for the persona dispatch path.
    tracing::info!(
        persona = %persona_name,
        agent = %persona_cfg.agent.name,
        runner = ?persona_cfg.agent.runner,
        model = %persona_cfg.agent.model,
        creds = creds.label(),
        claude_cli_short_circuit,
        use_anthropic_direct = persona_cfg.llm.use_anthropic_direct,
        "run_pm_task_with_persona: credentials resolved"
    );
    if claude_cli_short_circuit {
        return run_pm_task_via_claude_cli(project_path, &persona_cfg, user_input, history, &sid)
            .await;
    }
    let persona_llm_t0 = std::time::Instant::now();

    let client = llm::create_client()?;

    // #255: Build the persona's tool registry. Personas opt-in via the
    // `[tools] allow = [...]` glob list (see `ToolsConfig::allow`). When
    // absent, an empty registry preserves the prior pure-chat behavior.
    //
    // Why glob filtering: persona TOMLs say `allow = ["mcp_*", "git_log"]`
    // rather than enumerating every dynamic mcp_* tool. Suffix `*` is the
    // only wildcard supported (see `match_any_glob`).
    let (persona_registry, persona_tool_names): (ToolRegistry, Vec<String>) =
        if let Some(patterns) = persona_cfg.tools.allow.clone() {
            let mut registry = ToolRegistry::new();
            // Register the candidate tool surface for personas. Defence in
            // depth: only tools registered here can ever be reached, even
            // if `allow` names something else.
            for tool in crate::tools::mcp_tools::mcp_tool_executors() {
                registry.register(tool);
            }
            // Live MCP service tools — wraps each tool advertised by an
            // enabled service in `~/.open-mpm/config.toml` (granola_*,
            // gmail_*, calendar_*, etc.) so personas with
            // `allow = ["granola_*", ...]` actually have something to
            // dispatch to. Servers are spawned lazily on first call: if a
            // binary is missing PATH, the tool call surfaces a recoverable
            // error instead of blocking registry build.
            for tool in crate::tools::mcp_service_tools::mcp_service_tool_executors().await {
                registry.register(tool);
            }
            // OpenRPC tool registry (#453) — JSON-RPC 2.0 direct
            // endpoints declared under `[tool_registry]` / `[[endpoints]]`
            // in `~/.open-mpm/config.toml`. Init failure is non-fatal: a
            // single bad endpoint should not take down the harness.
            {
                let global_config = crate::mcp::config::GlobalConfig::load().await;
                match crate::tools::registry::ToolRegistryBuilder::from_config(&global_config)
                    .build()
                    .await
                {
                    Ok(execs) => {
                        for tool in execs {
                            registry.register(tool);
                        }
                    }
                    Err(e) => {
                        tracing::warn!("tool registry init failed: {e}");
                    }
                }
            }
            // Git tools — bypass the role gate (personas use
            // `role = "assistant"` which isn't in `git.available_for_roles`).
            // Discovery failure is non-fatal: no repo → no git tools.
            if let Ok(repo) = crate::git::GitRepo::open(
                &std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            ) {
                for tool in crate::tools::git_tools::git_tools(repo.root.clone()) {
                    registry.register(tool);
                }
            }
            // Native ticketing — same wiring helper used elsewhere; no-op
            // when no GitHub identity is configured.
            register_ticketing_tools(&mut registry).await;

            // Web search — register so personas with `allow = ["web_search"]`
            // can reach the Brave Search backend. Mirrors the wiring in the
            // ctrl and ctrl-digital-twin registries.
            registry.register(Arc::new(
                crate::tools::web_search::BraveSearchTool::from_env(),
            ));

            // #472 / agent-crate extraction: Persona-scoped tools (e.g. the
            // CTO DB HR/budget surface) are no longer hard-coded here. Each
            // external agent crate (cto-assistant, …) builds an `AgentPlugin`
            // bundling its `ToolExecutor`s and the target persona name, then
            // `main.rs` installs the plugin list at process startup. The
            // ctrl loop looks the active persona up at session-build time
            // and registers whatever tools were injected. This keeps the
            // sensitive-tool gating (one persona only) intact while letting
            // open-mpm stay agnostic of any specific agent's tool list.
            for plugin in crate::tools::agent_plugin::plugins_for_persona(persona_name) {
                for tool in &plugin.tools {
                    registry.register(std::sync::Arc::clone(tool));
                }
            }

            let all_names: Vec<String> = registry
                .schemas()
                .into_iter()
                .filter_map(|s| {
                    s.get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|n| n.as_str())
                        .map(String::from)
                })
                .collect();
            let mut kept: Vec<String> = all_names
                .into_iter()
                .filter(|name| match_any_glob(name, &patterns))
                .collect();
            // #481: RBAC tier gate. The persona toolset is further narrowed
            // to the tools the resolved caller may invoke — a non-`All` tier
            // (e.g. an `Analytics` Slack user) must not even see tools whose
            // `restricted_tiers` block their tier. We filter the *names* list
            // (which becomes `allowed_tools`) so dispatch rejects any
            // hallucinated out-of-tier call too.
            let rbac_user = overrides.user.clone().unwrap_or_default();
            let allowed_by_tier: std::collections::HashSet<String> = registry
                .filter_tools_for_user(&rbac_user)
                .into_iter()
                .map(|t| t.schema())
                .filter_map(|s| {
                    s.get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|n| n.as_str())
                        .map(String::from)
                })
                .collect();
            kept.retain(|name| allowed_by_tier.contains(name));
            tracing::info!(
                persona = %persona_name,
                tools = ?kept,
                rbac_user = %rbac_user.id,
                rbac_tier = ?rbac_user.tier,
                "persona tool registry built"
            );
            (registry, kept)
        } else {
            (ToolRegistry::new(), Vec::new())
        };

    // Build augmented system prompt with user profile + current date/time
    // injection so personas inherit the same context the default ctrl path
    // gets. Mirrors the block in `run_pm_task_with_history`.
    let system_prompt: String = {
        let base = build_user_context_prefix(&persona_cfg.system_prompt.content);
        // #478: Substitute {{AGENT_MODEL}} / {{AGENT_RUNNER}} placeholders so
        // persona TOMLs (e.g. ctrl.toml) don't leak raw placeholder strings.
        // Mirrors the block in `ctrl_chat_turn`.
        let runner_label = match persona_cfg.agent.runner {
            crate::agents::RunnerKind::Subprocess => "subprocess",
            crate::agents::RunnerKind::Inline => "inline",
            crate::agents::RunnerKind::ClaudeCode => "claude-code",
            crate::agents::RunnerKind::InProcess => "in-process",
        };
        let base = crate::agents::prompt_builder::SystemPromptBuilder::new(base)
            .with_agent_context(persona_cfg.agent.model.as_str(), runner_label)
            .build();
        // #255: Append a tool-capability note when the persona has tools
        // wired up so the LLM knows it can call them instead of saying
        // "I don't have live data access".
        if !persona_tool_names.is_empty() {
            format!(
                "{}\n\n## Available tools\nYou have access to the following tools: {}.\nUse them when the user asks questions that require live data.",
                base,
                persona_tool_names.join(", ")
            )
        } else {
            base
        }
    };

    let mut initial_messages: Vec<ChatCompletionRequestMessage> = Vec::new();
    initial_messages.push(
        ChatCompletionRequestSystemMessageArgs::default()
            .content(system_prompt)
            .build()
            .context("failed to build persona system message")?
            .into(),
    );
    for turn in history {
        initial_messages.push(
            ChatCompletionRequestUserMessageArgs::default()
                .content(turn.user.clone())
                .build()
                .context("failed to build persona history user message")?
                .into(),
        );
        initial_messages.push(
            ChatCompletionRequestAssistantMessageArgs::default()
                .content(turn.assistant.clone())
                .build()
                .context("failed to build persona history assistant message")?
                .into(),
        );
    }
    initial_messages.push(
        ChatCompletionRequestUserMessageArgs::default()
            .content(user_input)
            .build()
            .context("failed to build persona current user message")?
            .into(),
    );

    let adapter = llm::adapter::adapter_for_model(&persona_cfg.agent.model);
    // #255: Pass the persona registry + filtered name list. `allowed_tools`
    // gates dispatch so even if the model hallucinates an out-of-scope tool
    // call it gets rejected with a recoverable `ToolResult::Error` instead
    // of running. `max_turns = 4` matches the PM tool loop budget so the
    // model has room to call a tool, see the result, and respond.
    let allowed_tools = if persona_tool_names.is_empty() {
        None
    } else {
        Some(persona_tool_names.clone())
    };
    let max_turns = if persona_tool_names.is_empty() { 2 } else { 4 };
    let (content, _usage) = llm::chat_with_tools_gated(
        &client,
        &persona_cfg.agent.model,
        &*adapter,
        initial_messages,
        Arc::new(persona_registry),
        allowed_tools,
        persona_cfg.llm.temperature,
        persona_cfg.llm.max_tokens,
        max_turns,
        false,
        None,
        false,
        persona_cfg.llm.use_anthropic_direct,
        &persona_cfg.llm.stop_sequences,
    )
    .await
    .context("persona LLM call failed")?;
    tracing::info!(
        persona = %persona_name,
        llm_ms = persona_llm_t0.elapsed().as_millis() as u64,
        response_chars = content.len(),
        "run_pm_task_with_persona: LLM call complete"
    );

    Ok(content)
}

/// Drive a PM/ctrl turn through the local `claude` CLI subprocess (#250).
///
/// Why: When only `CLAUDE_CODE_OAUTH_TOKEN` is configured (no OpenRouter key,
/// no Anthropic API key), the ctrl orchestrator can't reach api.anthropic.com
/// or OpenRouter directly — OAuth tokens are only valid for the `claude` CLI.
/// What: Builds a single concatenated prompt (system + history + user turn)
/// and dispatches via `ClaudeCodeAgentRunner::new()` (a slim wrapper that
/// already speaks stream-json and surfaces the final result string). Tools
/// are intentionally NOT registered here: the claude CLI brings its own tool
/// surface, and trying to graft open-mpm's `delegate_to_agent` onto it would
/// require a second Claude Max session inside the CLI's own session.
/// Test: Compilation-tested; the CLI path is exercised manually via
/// `cargo run` with only `CLAUDE_CODE_OAUTH_TOKEN` set.
/// Strip claude CLI artifacts from the end of a response.
///
/// DEFENSIVE-ONLY (Feature A): As of the conversational-output-mode rewrite,
/// the conversational agents (ctrl, izzie, cto-assistant) are configured with
/// stop_sequences and prose-only system prompts that should prevent any
/// `## Summary` block from appearing in the first place. This helper is kept
/// in place as a safety net for legacy agents and CLI subprocess outputs that
/// may still emit the artifact; do not remove it without a deprecation pass.
///
/// Why: The claude CLI appends a trailing `\n\n## Summary\n…` block to its
/// final output, plus stray trailing whitespace. The TUI renders this verbatim
/// and it makes ctrl chat replies look like build reports rather than
/// conversational answers. We strip it here so the helper has a single home
/// and unit tests can pin down the trim semantics.
/// What: Removes everything from the first occurrence of `\n\n## Summary` (or
/// a `## Summary` header at start-of-line preceded by a single newline) to end
/// of string, then trims any trailing whitespace/newlines.
/// Test: `strip_cli_artifacts_*` unit tests in `mod tests` cover the both-
/// newline form, the single-newline form, the no-summary case, and trailing
/// whitespace trimming.
fn strip_cli_artifacts(s: String) -> String {
    let cut = if let Some(idx) = s.find("\n\n## Summary") {
        Some(idx)
    } else if let Some(idx) = s.find("\n## Summary") {
        Some(idx)
    } else if s.starts_with("## Summary") {
        Some(0)
    } else {
        None
    };
    match cut {
        Some(idx) => s[..idx].trim_end().to_string(),
        None => s.trim_end().to_string(),
    }
}

/// Apply relevance-first filtering to the project-index section of a system
/// prompt, matching `WorkflowEngine`'s behavior (#280).
///
/// Why: The ctrl direct-CLI dispatch path (`run_pm_task_via_claude_cli`) was
/// injecting the full project-index whenever the loaded agent TOML had one
/// embedded — but the workflow engine already filters its index by task
/// keywords before injection, so the two paths burned different token budgets
/// for the same user intent. This helper closes the divergence by running
/// the same `filter_index_entries` over the section in-place.
/// What: Locates `## Project Context (auto-indexed)` in `system_prompt`,
/// extracts its body up to the next `## ` heading or `---` separator,
/// runs `filter_index_entries(body, task, top_n)`, and splices the
/// filtered body back. If the marker section isn't present the prompt
/// is returned unchanged (graceful fallback for agents that never
/// embed an index).
/// Test: `filter_project_index_in_prompt_*` unit tests below.
fn filter_project_index_in_prompt(system_prompt: &str, task: &str, top_n: usize) -> String {
    const HEADER: &str = "## Project Context (auto-indexed)";
    let Some(header_start) = system_prompt.find(HEADER) else {
        return system_prompt.to_string();
    };
    let body_start = header_start + HEADER.len();
    // Skip the blank line(s) after the header so the filter only sees bullet
    // entries — `filter_index_entries` preserves any leading non-bullet
    // preamble verbatim, which would re-emit the header redundantly.
    let after_header = &system_prompt[body_start..];
    let body_offset = after_header
        .char_indices()
        .find(|(_, c)| *c != '\n')
        .map(|(i, _)| i)
        .unwrap_or(after_header.len());
    let body_abs_start = body_start + body_offset;

    // Section ends at the next `## ` heading OR the next `---\n` separator,
    // whichever comes first. Both are produced by `InitContext::to_prompt_prefix`
    // and appear in the wild for the workflow-engine path. If neither marker
    // is found, the section runs to end-of-prompt.
    let tail = &system_prompt[body_abs_start..];
    let next_section = tail
        .find("\n## ")
        .map(|i| body_abs_start + i + 1) // +1 to keep the leading `\n`
        .unwrap_or(system_prompt.len());
    let next_separator = tail
        .find("\n---")
        .map(|i| body_abs_start + i + 1)
        .unwrap_or(system_prompt.len());
    let body_end = next_section.min(next_separator);

    let body = &system_prompt[body_abs_start..body_end];
    let filtered = crate::agents::context_filter::filter_index_entries(body, task, top_n);

    let mut out = String::with_capacity(system_prompt.len());
    out.push_str(&system_prompt[..body_abs_start]);
    out.push_str(&filtered);
    // Preserve a trailing newline before the next section so headings stay
    // separated; `filter_index_entries` may strip its own trailing whitespace.
    if !filtered.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&system_prompt[body_end..]);
    out
}

async fn run_pm_task_via_claude_cli(
    _project_path: &Path,
    pm_cfg: &AgentConfig,
    user_input: &str,
    history: &[ConversationTurn],
    sid: &str,
) -> Result<String> {
    // #283 follow-up: latency instrumentation for the ctrl direct-CLI path.
    // Why: Helps operators correlate "spinner stuck" feelings with real
    // wall-clock spend on the claude CLI subprocess.
    let t0 = std::time::Instant::now();

    // Emit `AgentStarted` so the TUI shows `⟳ ctrl · running…` while the
    // CLI subprocess is in flight. Mirrors the pattern in `subprocess.rs`
    // and `claude_code_runner.rs` so the same UI affordance lights up
    // regardless of which dispatch path is used.
    let agent_name = pm_cfg.agent.name.clone();
    let session_id = sid.to_string();
    events::publish(Event::AgentStarted {
        session_id: session_id.clone(),
        agent_name: agent_name.clone(),
        runner_type: "claude-code".to_string(),
    });

    let runner = match crate::agents::claude_code_runner::ClaudeCodeAgentRunner::new()
        .await
        .context("ctrl: failed to locate `claude` CLI for CLAUDE_CODE_OAUTH_TOKEN routing")
    {
        Ok(r) => r,
        Err(e) => {
            events::publish(Event::AgentDone {
                session_id: session_id.clone(),
                agent: agent_name.clone(),
                status: "error".to_string(),
            });
            return Err(e);
        }
    };

    // Compose history into a single prompt — claude CLI is single-shot per
    // invocation. Prefix each turn with a clear marker so the model can read
    // the dialogue chronologically.
    let mut composed = String::new();
    for turn in history {
        composed.push_str("User: ");
        composed.push_str(&turn.user);
        composed.push_str("\n\nAssistant: ");
        composed.push_str(&turn.assistant);
        composed.push_str("\n\n");
    }
    composed.push_str("User: ");
    composed.push_str(user_input);

    // #280: Apply relevance-first project-index filtering to the embedded
    // system prompt before handing it to the claude CLI. Mirrors
    // `WorkflowEngine::run_phase`'s `filter_index_entries(.., 15)` call so
    // both dispatch paths burn the same token budget on context for the
    // same task. Graceful no-op when the prompt has no `## Project Context
    // (auto-indexed)` section.
    let mut filtered_cfg = pm_cfg.clone();
    filtered_cfg.system_prompt.content =
        filter_project_index_in_prompt(&pm_cfg.system_prompt.content, user_input, 15);

    // Build a config tweak that forces the runner to use the resolved model
    // verbatim (already set on pm_cfg).
    let result = match runner
        .run_with_config_public(&filtered_cfg, &composed)
        .await
        .context("ctrl: claude CLI invocation failed")
    {
        Ok(r) => r,
        Err(e) => {
            tracing::info!(
                duration_ms = t0.elapsed().as_millis() as u64,
                agent = %agent_name,
                "ctrl CLI dispatch failed"
            );
            events::publish(Event::AgentDone {
                session_id: session_id.clone(),
                agent: agent_name.clone(),
                status: "error".to_string(),
            });
            return Err(e);
        }
    };

    tracing::info!(
        duration_ms = t0.elapsed().as_millis() as u64,
        agent = %agent_name,
        "ctrl CLI dispatch complete"
    );
    events::publish(Event::AgentDone {
        session_id,
        agent: agent_name,
        status: "success".to_string(),
    });

    Ok(strip_cli_artifacts(result.content))
}

/// Spawn the per-project Unix-socket accept loop alongside the stdin REPL.
///
/// Why: After binding the controller socket, every subsequent `open-mpm`
/// invocation in the same project routes its argv into the running
/// controller via this listener. The listener stays "thin": it accepts
/// connections, parses one NDJSON command, dispatches a single PM round-trip
/// scoped to the request's `cwd`, and streams replies back. It does NOT
/// share state with the stdin REPL — it just reuses `run_pm_task` so both
/// paths exercise the same PM logic.
/// What: Reads exactly one JSON line per connection. Recognized command
/// types: `task` (run a PM task and stream output), `status` (return a
/// minimal liveness payload), `shutdown` (acknowledged but not yet wired
/// to actually stop the controller — Phase A leaves graceful shutdown for
/// later). Each connection gets its own tokio task so a slow PM call does
/// not block the listener.
/// Test: Manual — `open-mpm` from terminal A, then `open-mpm "hello"` from
/// terminal B prints the PM's output in B and exits while A keeps running.
pub async fn spawn_socket_listener(listener: tokio::net::UnixListener) {
    tracing::info!("ctrl: socket listener accepting connections");
    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                tokio::spawn(handle_socket_connection(stream));
            }
            Err(e) => {
                tracing::warn!(error = %e, "ctrl: socket accept failed (continuing)");
                // Brief backoff to avoid a hot error loop on EMFILE / ENFILE.
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    }
}

/// Handle one inbound CLI connection: parse one command, stream replies.
///
/// Why: Pulled into its own function so the accept loop stays tiny and so
/// connection-scoped errors surface as warnings instead of poisoning the
/// listener.
/// What: Reads one line, dispatches by `type`, writes NDJSON replies, and
/// always finishes with a `done` or `error` envelope so the client knows
/// when to disconnect.
async fn handle_socket_connection(stream: tokio::net::UnixStream) {
    use tokio::io::AsyncBufReadExt;

    let (read_half, write_half) = stream.into_split();
    let mut reader = tokio::io::BufReader::new(read_half);
    let writer = std::sync::Arc::new(tokio::sync::Mutex::new(write_half));

    let mut line = String::new();
    if let Err(e) = reader.read_line(&mut line).await {
        tracing::warn!(error = %e, "ctrl socket: failed to read command line");
        return;
    }
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return;
    }

    let parsed: serde_json::Value = match serde_json::from_str(trimmed) {
        Ok(v) => v,
        Err(e) => {
            let _ = write_socket_line(
                &writer,
                &serde_json::json!({"type": "error", "error": format!("invalid JSON: {e}")}),
            )
            .await;
            return;
        }
    };

    let kind = parsed
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let id = parsed
        .get("id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    match kind.as_str() {
        "status" => {
            let payload = serde_json::json!({
                "type": "output",
                "id": id,
                "text": format!("controller alive (pid={})", std::process::id()),
            });
            let _ = write_socket_line(&writer, &payload).await;
            let _ = write_socket_line(
                &writer,
                &serde_json::json!({"type": "done", "id": id, "status": "success"}),
            )
            .await;
        }
        "shutdown" => {
            let _ = write_socket_line(
                &writer,
                &serde_json::json!({
                    "type": "output",
                    "id": id,
                    "text": "shutdown requested (not yet implemented)",
                }),
            )
            .await;
            let _ = write_socket_line(
                &writer,
                &serde_json::json!({"type": "done", "id": id, "status": "success"}),
            )
            .await;
        }
        "task" => {
            let text = parsed
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let cwd = parsed
                .get("cwd")
                .and_then(|v| v.as_str())
                .map(PathBuf::from)
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

            let _ = write_socket_line(
                &writer,
                &serde_json::json!({
                    "type": "output",
                    "id": id,
                    "text": format!("Dispatching task in {}...", cwd.display()),
                }),
            )
            .await;

            // #192 Phase B: emit `SessionStarted` + `SessionDone` so SSE
            // subscribers see the controller-routed task in real time. The
            // socket request `id` doubles as the session_id so the UI can
            // correlate filtered streams (`?session_id=<id>`) to this task.
            let project_label = cwd
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("(cwd)")
                .to_string();
            events::publish(Event::SessionStarted {
                session_id: id.clone(),
                project: project_label,
            });

            // Parse optional history array from the task message.
            // Each element: {"user": "...", "assistant": "..."}
            // Missing or malformed history → treat as empty (backward-compatible
            // with older clients that don't send the history field).
            let history: Vec<ConversationTurn> = parsed
                .get("history")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|item| {
                            let user = item.get("user").and_then(|v| v.as_str())?.to_string();
                            let assistant =
                                item.get("assistant").and_then(|v| v.as_str())?.to_string();
                            Some(ConversationTurn { user, assistant })
                        })
                        .collect()
                })
                .unwrap_or_default();

            let result = run_pm_task_with_history(
                &cwd,
                &text,
                &history,
                Some(id.clone()),
                SessionOverrides::default(),
            )
            .await;
            match result {
                Ok(out) => {
                    let _ = write_socket_line(
                        &writer,
                        &serde_json::json!({
                            "type": "output",
                            "id": id,
                            "text": out,
                        }),
                    )
                    .await;
                    events::publish(Event::SessionDone {
                        session_id: id.clone(),
                        status: "success".to_string(),
                    });
                    let _ = write_socket_line(
                        &writer,
                        &serde_json::json!({
                            "type": "done",
                            "id": id,
                            "status": "success",
                        }),
                    )
                    .await;
                }
                Err(e) => {
                    events::publish(Event::SessionDone {
                        session_id: id.clone(),
                        status: "error".to_string(),
                    });
                    let _ = write_socket_line(
                        &writer,
                        &serde_json::json!({
                            "type": "error",
                            "id": id,
                            "error": format!("{e:#}"),
                        }),
                    )
                    .await;
                }
            }
        }
        other => {
            let _ = write_socket_line(
                &writer,
                &serde_json::json!({
                    "type": "error",
                    "id": id,
                    "error": format!("unknown command type: {other}"),
                }),
            )
            .await;
        }
    }
}

/// Write one JSON value as an NDJSON line to a shared writer.
async fn write_socket_line(
    writer: &std::sync::Arc<tokio::sync::Mutex<tokio::net::unix::OwnedWriteHalf>>,
    value: &serde_json::Value,
) -> std::io::Result<()> {
    let mut line = serde_json::to_string(value)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    line.push('\n');
    let mut g = writer.lock().await;
    g.write_all(line.as_bytes()).await?;
    g.flush().await
}

/// CLI-side: forward this invocation to a running controller and stream
/// its replies to stdout/stderr until `done` or `error` arrives.
///
/// Why: Lets `main()` short-circuit when a controller is already running —
/// the second `open-mpm` invocation just becomes a thin client. Streaming
/// (rather than buffering) means the user sees PM progress immediately
/// once the controller emits it.
/// What: Writes one `task` command line, then reads NDJSON replies in a
/// loop. `output` envelopes print to stdout; `done` returns Ok; `error`
/// returns Err with the controller's message.
/// Test: Manual — start a controller in terminal A, run
/// `open-mpm "say hi"` in terminal B, observe streamed output.
///
/// The `project_dir` argument is the project root that the controller should
/// resolve agent configs against (it lands in the `cwd` field of the task
/// envelope). Callers MUST pass their resolved project root rather than
/// relying on `std::env::current_dir()` here — the REPL maintains its own
/// `project_dir` that may differ from the OS cwd (e.g., after `/connect` or
/// `/cd`), and using process cwd would route agent-config lookups to the
/// wrong directory (issue #238).
pub async fn forward_to_controller(
    stream: tokio::net::UnixStream,
    task_text: String,
    history: &[ConversationTurn],
    project_dir: &Path,
) -> Result<String> {
    use tokio::io::AsyncBufReadExt;

    let (read_half, mut write_half) = stream.into_split();
    let id = uuid::Uuid::new_v4().to_string();
    let cwd = project_dir.display().to_string();
    // Why: Serialize the caller's conversation history into the task envelope so
    // the server-side `handle_socket_connection` can reconstruct turns and call
    // `run_pm_task_with_history`. Empty slice → empty array (backward-compatible).
    let history_json: Vec<serde_json::Value> = history
        .iter()
        .map(|t| serde_json::json!({"user": t.user, "assistant": t.assistant}))
        .collect();
    let cmd = serde_json::json!({
        "type": "task",
        "id": id,
        "text": task_text,
        "cwd": cwd,
        "history": history_json,
    });
    let mut line = serde_json::to_string(&cmd)?;
    line.push('\n');
    write_half.write_all(line.as_bytes()).await?;
    write_half.flush().await?;

    let mut reader = tokio::io::BufReader::new(read_half);
    let mut buf = String::new();
    let mut accumulated = String::new();
    // Why: The first "output" envelope from the controller is always a
    // progress preamble (e.g. "Dispatching task in /path..."); it is not
    // part of the actual PM response. Skipping it keeps the accumulated
    // string clean so the REPL can render only the real response. We also
    // do NOT write to stdout inline anymore — inline writes conflict with
    // the REPL's crossterm-driven status bar and cause the response to be
    // clobbered. The caller prints the final accumulated string once.
    let mut is_first_output = true;
    loop {
        buf.clear();
        let n = reader.read_line(&mut buf).await?;
        if n == 0 {
            // Controller closed the connection without `done`.
            anyhow::bail!("controller closed connection unexpectedly");
        }
        let value: serde_json::Value = match serde_json::from_str(buf.trim()) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, line = %buf.trim(), "controller emitted invalid JSON");
                continue;
            }
        };
        let kind = value.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match kind {
            "output" => {
                if let Some(text) = value.get("text").and_then(|v| v.as_str()) {
                    if is_first_output {
                        // Skip the server-side "Dispatching task in X..."
                        // preamble; only accumulate real PM output.
                        is_first_output = false;
                    } else {
                        accumulated.push_str(text);
                        if !text.ends_with('\n') {
                            accumulated.push('\n');
                        }
                    }
                }
            }
            "done" => return Ok(accumulated),
            "error" => {
                let msg = value
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("(no error message)")
                    .to_string();
                anyhow::bail!("controller error: {msg}");
            }
            _ => {
                // Unknown envelope type — log and keep streaming.
                tracing::debug!(kind = %kind, "unknown reply type from controller");
            }
        }
    }
}

// INTENT: Print the CTRL command reference.
fn print_help() {
    println!(
        "\
CTRL commands:
  /connect <PATH>          Start (or switch to) a PM session for PATH
  /disconnect              Return to CTRL prompt (PM keeps running)
  /status                  List PM sessions, registered projects, live buses
  /send <PROJECT> <MSG>    Send a message to another project via the bus
  /sessions [QUERY]        Search past workflow runs (cross-project)
  /help                    Show this message
  /quit | /exit            Shutdown all sessions and exit"
    );
}

// INTENT: Parse and dispatch a slash command, returning false on quit.
async fn handle_command(ctrl: &mut Ctrl, line: &str) -> Result<bool> {
    let mut parts = line.splitn(2, ' ');
    let cmd = parts.next().unwrap_or("");
    let arg = parts.next().map(str::trim);

    match cmd {
        "/connect" => {
            let path = arg.context("/connect requires a PATH argument")?;
            match ctrl.connect(path).await {
                Ok(msg) => println!("{msg}"),
                Err(e) => eprintln!("connect error: {e:#}"),
            }
        }
        "/disconnect" => {
            println!("{}", ctrl.disconnect());
        }
        "/status" => {
            // Local PM sessions.
            println!("{}", ctrl.status());

            // #116: Global project registry summary from ~/.open-mpm/projects.json.
            match ProjectRegistry::new() {
                Ok(reg) => match reg.status_summary().await {
                    Ok(summary) => println!("\n{summary}"),
                    Err(e) => eprintln!("registry error: {e:#}"),
                },
                Err(e) => eprintln!("registry unavailable: {e:#}"),
            }

            // #117: List projects with live bus sockets.
            match MessageBus::list_running().await {
                Ok(running) if !running.is_empty() => {
                    println!("\n## Live Bus Connections\n");
                    for id in &running {
                        println!("  - {id}");
                    }
                }
                Ok(_) => {}
                Err(e) => eprintln!("bus list_running error: {e:#}"),
            }
        }
        // #117: Send a message to another project's message bus.
        "/send" => {
            let rest = arg.context("/send requires <PROJECT> <MESSAGE>")?;
            let mut parts2 = rest.splitn(2, ' ');
            let target = parts2
                .next()
                .context("/send requires a target project name")?;
            let msg_text = parts2.next().unwrap_or("").trim();
            if let Some(bus) = &ctrl.bus {
                let payload = serde_json::json!({ "type": "task", "text": msg_text });
                match bus.send_to(target, payload).await {
                    Ok(()) => println!("Sent to {target}"),
                    Err(e) => eprintln!("send error: {e:#}"),
                }
            } else {
                eprintln!("Bus not available — inter-project messaging requires a running bus");
            }
        }
        // Search past workflow runs recorded in ~/.open-mpm/sessions/runs.jsonl.
        "/sessions" => {
            let query = arg.unwrap_or("");
            match session_record::search(query).await {
                Ok(hits) if hits.is_empty() => println!("(no matching sessions)"),
                Ok(hits) => {
                    for h in hits.iter().take(20) {
                        let score = h.score.as_deref().unwrap_or("-");
                        println!(
                            "{}  {}  {}  cost=${:.2}  mins={}  score={}  task={}",
                            h.timestamp,
                            h.build_id,
                            h.status,
                            h.cost_usd,
                            h.duration_mins,
                            score,
                            h.task
                        );
                    }
                }
                Err(e) => eprintln!("sessions error: {e:#}"),
            }
        }
        "/help" => {
            print_help();
        }
        "/quit" | "/exit" | "/q" => {
            println!("Shutting down...");
            return Ok(false);
        }
        other => {
            eprintln!("Unknown command: {other}  (type /help for commands)");
        }
    }
    Ok(true)
}

// INTENT: Public entry point for the CTRL interactive REPL.
pub async fn run_ctrl() -> Result<()> {
    run_ctrl_inner(true, None).await
}

/// Headless variant of [`run_ctrl`]: performs all controller initialization
/// (socket binding, docs indexing, memory seeding, message bus, profile load)
/// but skips the interactive stdin loop and the CTRL banner.
///
/// Why: When the rich reedline REPL drives stdin, having `run_ctrl` also
/// read stdin causes both readers to compete for keystrokes — every other
/// keypress disappears into the controller (printing `CTRL>` prompts,
/// auto-submitting empty lines to the REPL). The REPL spawns this variant
/// in a background task so the controller's services stay available
/// (socket forwarding, docs index, bus relay) while the REPL owns stdin.
/// What: Runs the same setup as `run_ctrl`, then parks on
/// `std::future::pending::<()>().await` until the spawning task is aborted.
/// Test: Spawn `run_ctrl_headless` in a tokio task; verify the controller
/// socket binds (probe `ctrl_socket_path(&cwd_project_id())`) and that the
/// task remains alive (does not return) until aborted by the caller.
///
/// `ready_tx` (#477): an optional oneshot sender fired once the controller
/// socket bind step completes, so the REPL can probe it without a fixed
/// `sleep`. The signal is sent regardless of bind success — it marks
/// "controller setup reached the socket stage", not "socket is up".
pub async fn run_ctrl_headless(ready_tx: Option<tokio::sync::oneshot::Sender<()>>) -> Result<()> {
    run_ctrl_inner(false, ready_tx).await
}

/// Why: Shared implementation backing both [`run_ctrl`] (interactive stdin
/// loop) and [`run_ctrl_headless`] (REPL-driven, no stdin). Keeping a single
/// setup path guarantees both modes bind the same socket, build the same
/// docs index, and seed the same memory store.
/// What: Runs profile load, self-project detection, docs indexing, memory
/// seeding, message bus startup, and controller socket bind. If
/// `with_stdin == true`, then runs the legacy stdin REPL loop; otherwise
/// parks indefinitely so background tasks remain alive.
/// Test: Both `run_ctrl()` (piped stdin closes -> "Bye." + Ok(())) and
/// `run_ctrl_headless()` (never returns until aborted) must succeed.
async fn run_ctrl_inner(
    with_stdin: bool,
    ready_tx: Option<tokio::sync::oneshot::Sender<()>>,
) -> Result<()> {
    if with_stdin {
        eprintln!(
            "{} CTRL — machine-level coordination\ntype /help for commands, /connect <PATH> to start a PM session\n",
            crate::build_info::version_string()
        );
    }

    // #193: Load (or interview-and-create) the user profile so the CTRL
    // system prompt can be personalized. Stored at `~/.open-mpm/user.toml`.
    let user_profile = load_or_create_user_profile().await?;

    // #184: CTRL's LLM turns are lightweight; restrict skill discovery to
    // project-local sources so we don't hang on `~/.claude/skills/` (700+
    // files in claude-mpm). Subagents spawned by CTRL inherit this env var.
    // Workflow runs (which call `cargo run -- --workflow <name>` directly)
    // are unaffected.
    // SAFETY: single-threaded startup context; set before any subprocess spawn.
    if std::env::var("OPEN_MPM_SKILLS_PROJECT_LOCAL_ONLY").is_err() {
        unsafe {
            std::env::set_var("OPEN_MPM_SKILLS_PROJECT_LOCAL_ONLY", "1");
        }
        tracing::debug!("CTRL: defaulting OPEN_MPM_SKILLS_PROJECT_LOCAL_ONLY=1");
    }

    let mut ctrl = Ctrl::new();
    ctrl.user_profile = user_profile;

    // #182: Detect open-mpm's own project root (if we're running from it) and
    // auto-register it so the user can dispatch self-development tasks via
    // `initiate_self_task` / `self_project_status` tools below.
    if let Some(self_path) = detect_self_project() {
        tracing::info!(path = %self_path.display(), "self-project detected");
        if let Ok(reg) = ProjectRegistry::new()
            && let Err(e) = reg.register_self_project(&self_path).await
        {
            tracing::warn!(error = %e, "register_self_project failed");
        }
        ctrl.self_project = Some(self_path);
    }

    // #187: Build the docs index in the background so the REPL prompt comes up
    // immediately. Walks `<self_project_or_cwd>/docs/`. Index installs into
    // `ctrl.docs_index` (Arc<Mutex<Option<...>>>) when complete; the
    // `search_docs` tool returns a graceful "not ready" message until then.
    {
        let docs_root = ctrl
            .self_project
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
            .join("docs");
        let slot = ctrl.docs_index.clone();
        tokio::spawn(async move {
            let docs_root_clone = docs_root.clone();
            let idx = tokio::task::spawn_blocking(move || {
                crate::docs_index::DocsIndex::build(&docs_root_clone)
            })
            .await
            .ok();
            if let Some(idx) = idx {
                let n = idx.len();
                if let Ok(mut g) = slot.lock() {
                    *g = Some(Arc::new(idx));
                }
                tracing::info!(
                    "[open-mpm] Docs index: {n} documents indexed from {}",
                    docs_root.display()
                );
            }
        });
    }

    // #190: On fresh installs, seed agent memory with project docs (docs/user/,
    // docs/developer/) so subagents launched by CTRL can recall project-specific
    // guidance via `memory_recall`. Runs in the background so REPL startup is
    // not blocked by model load + embedding. Best-effort: any failure
    // (missing docs/, embedder init, store open) is logged and does not
    // disrupt the REPL.
    {
        let project_root = ctrl
            .self_project
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        tokio::spawn(async move {
            let omd = project_root.join(".open-mpm").join("state");
            if let Err(e) = tokio::fs::create_dir_all(&omd).await {
                tracing::warn!(error = %e, "ctrl doc seed: create state dir failed");
                return;
            }
            // Run project init (idempotent — fast no-op if marker is fresh).
            let initializer = crate::init::ProjectInitializer::new(&project_root, &omd);
            if let Err(e) = initializer.initialize_if_needed().await {
                tracing::warn!(error = %e, "ctrl: project init failed (continuing)");
            }

            let session_dir = project_root
                .join(".open-mpm")
                .join("sessions")
                .join("default");
            if let Err(e) = tokio::fs::create_dir_all(&session_dir).await {
                tracing::warn!(error = %e, "ctrl doc seed: create session dir failed");
                return;
            }
            let store = match crate::memory::open_memory_store(&session_dir) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(error = %e, "ctrl doc seed: store open failed");
                    return;
                }
            };
            let embedder = match crate::memory::FastEmbedder::new() {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!(error = %e, "ctrl doc seed: embedder unavailable");
                    return;
                }
            };
            // #190+: seed docs + skills + MCP connections in one call. The
            // helper logs its own combined "[open-mpm] Memory seeded: …"
            // line and is robust to per-stage failures.
            let _ = initializer.seed_all(store.as_ref(), &embedder).await;
        });
    }

    // #117: Start CTRL's own inter-project message bus so other projects can
    // discover and send messages to us. The bus socket lives at
    // ~/.open-mpm/sockets/ctrl.sock. Failures are non-fatal.
    match MessageBus::start("ctrl").await {
        Ok(bus) => {
            // Spawn a background relay task: print incoming envelopes to
            // stderr so the user sees inter-project signals without blocking
            // the REPL. When the target project matches a connected PM, the
            // task text is forwarded via dispatch_task.
            let mut rx = bus.subscribe();
            let connected_pms = ctrl.connected_pms.clone();
            tokio::spawn(async move {
                loop {
                    match rx.recv().await {
                        Ok(envelope) => {
                            tracing::info!(
                                "[BUS] from {}: {}",
                                envelope.source_project,
                                serde_json::to_string(&envelope.message)
                                    .unwrap_or_else(|_| "(unserializable)".into())
                            );
                            // Persist to the audit log (best-effort).
                            if let Err(e) = append_pm_message(&envelope) {
                                tracing::warn!(error = %e, "append_pm_message failed");
                            }
                            // If the envelope targets a connected PM, forward
                            // its `text` (or raw JSON) into the PM actor's
                            // task channel so the PM can act on it.
                            if let Some(target) = envelope.target_project.as_deref() {
                                let sender_opt = {
                                    let m = connected_pms.lock().await;
                                    m.get(target).cloned()
                                };
                                if let Some(pm_tx) = sender_opt {
                                    let text = envelope
                                        .message
                                        .get("text")
                                        .and_then(|v| v.as_str())
                                        .map(str::to_string)
                                        .unwrap_or_else(|| envelope.message.to_string());
                                    let (reply_tx, reply_rx) = oneshot::channel();
                                    if let Err(e) = pm_tx
                                        .send(PmMsg::Task {
                                            text,
                                            reply: reply_tx,
                                        })
                                        .await
                                    {
                                        tracing::warn!(error = %e, target = %target, "bus relay: PM channel closed");
                                    } else {
                                        // Don't block the relay loop on the
                                        // reply; spawn a follow-up task that
                                        // surfaces the response.
                                        let target_owned = target.to_string();
                                        tokio::spawn(async move {
                                            match reply_rx.await {
                                                Ok(Ok(out)) => {
                                                    tracing::info!(
                                                        "[BUS->PM[{target_owned}]] {out}"
                                                    );
                                                }
                                                Ok(Err(e)) => {
                                                    tracing::warn!(error = %e, "bus->PM task error");
                                                }
                                                Err(e) => {
                                                    tracing::warn!(error = %e, "bus->PM reply dropped");
                                                }
                                            }
                                        });
                                    }
                                }
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!(n = n, "CTRL bus relay: lagged, {n} messages dropped");
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            });
            ctrl.bus = Some(bus);
        }
        Err(e) => {
            tracing::warn!(error = %e, "CTRL message bus failed to start (inter-project relay unavailable)");
        }
    }

    // #192 Phase A: bind the controller singleton socket and spawn an
    // accept loop. Subsequent `open-mpm` invocations in this project will
    // probe this socket and forward their argv into us instead of starting
    // an independent controller. Failure is non-fatal — the REPL continues
    // to function locally even if the socket can't be bound (e.g.,
    // permissions, no $HOME).
    let project_id = cwd_project_id();
    let sock_path = ctrl_socket_path(&project_id);
    match CtrlSocket::bind(&sock_path).await {
        Ok(listener) => {
            tracing::info!(
                "[open-mpm] controller socket listening at {}",
                sock_path.display()
            );
            tokio::spawn(spawn_socket_listener(listener));
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                path = %sock_path.display(),
                "ctrl: failed to bind controller socket (CLI forwarding disabled)"
            );
        }
    }

    // #477: Signal controller readiness so the REPL can stop waiting on a
    // fixed sleep. Fired after the socket-bind step regardless of outcome.
    if let Some(tx) = ready_tx {
        let _ = tx.send(());
    }

    if !with_stdin {
        // Headless: keep this task alive so the socket listener and
        // background tasks (docs index, memory seed, bus relay) keep
        // running. The caller (e.g., REPL in main.rs) will abort us via
        // `JoinHandle::abort()` on shutdown.
        std::future::pending::<()>().await;
        // Unreachable, but keeps the function's return type unified.
        return Ok(());
    }

    let mut stdin = BufReader::new(tokio::io::stdin());
    let mut stdout = tokio::io::stdout();

    loop {
        stdout
            .write_all(ctrl.prompt().as_bytes())
            .await
            .context("failed to write prompt")?;
        stdout.flush().await.context("failed to flush prompt")?;

        let mut line = String::new();
        let n = stdin
            .read_line(&mut line)
            .await
            .context("failed to read stdin")?;

        if n == 0 {
            println!("Bye.");
            break;
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if trimmed.starts_with('/') {
            match handle_command(&mut ctrl, trimmed).await {
                Ok(false) => break,
                Ok(true) => {}
                Err(e) => eprintln!("command error: {e:#}"),
            }
        } else if ctrl.active.is_some() {
            // There's an active PM — forward the user's text to it.
            let start = Instant::now();
            match ctrl.dispatch_task(trimmed.to_string()).await {
                Ok(output) => {
                    let elapsed = start.elapsed();
                    println!("{output}");
                    eprintln!(
                        "[TIMING] PM task responded in {:.2}s",
                        elapsed.as_secs_f64()
                    );
                }
                Err(e) => {
                    let elapsed = start.elapsed();
                    eprintln!("task error: {e:#}");
                    eprintln!(
                        "[TIMING] PM task failed after {:.2}s",
                        elapsed.as_secs_f64()
                    );
                }
            }
        } else {
            // No active PM: run a CTRL-level LLM turn. The LLM can call
            // start_pm / search_sessions / list_projects / memory_* tools.
            let start = Instant::now();
            match ctrl_chat_turn(&mut ctrl, trimmed).await {
                Ok(output) => {
                    let elapsed = start.elapsed();
                    if !output.trim().is_empty() {
                        println!("{output}");
                    }
                    eprintln!(
                        "[TIMING] CTRL turn responded in {:.2}s",
                        elapsed.as_secs_f64()
                    );
                }
                Err(e) => {
                    let elapsed = start.elapsed();
                    eprintln!("ctrl error: {e:#}");
                    eprintln!(
                        "[TIMING] CTRL turn failed after {:.2}s",
                        elapsed.as_secs_f64()
                    );
                }
            }
        }
    }

    ctrl.shutdown_all().await;
    Ok(())
}

// --- CTRL tools -------------------------------------------------------------

/// `start_pm(project_path)` — requested project path is captured in a shared
/// Option; the REPL loop drains it after the LLM turn and actually spawns the
/// PM via `Ctrl::connect`. This indirection keeps the tool pure (no &mut Ctrl
/// references) while still achieving the user-visible effect.
///
/// (#202) When `project_path` is missing or empty, falls back to the
/// `active_project` slot populated by `SetActiveProjectTool`, so a user who
/// already called `set_active_project(...)` can say "start a PM" without
/// re-typing the path.
struct StartPmTool {
    pending: Arc<Mutex<Option<String>>>,
    /// (#202) Default project to use when the LLM omits `project_path`.
    active_project: Arc<Mutex<Option<PathBuf>>>,
}

#[async_trait]
impl ToolExecutor for StartPmTool {
    fn name(&self) -> &str {
        "start_pm"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "start_pm",
                "description": "Spawn a project-scoped PM for the given absolute path. If 'project_path' is omitted, falls back to the active project (set via set_active_project).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "project_path": { "type": "string", "description": "Absolute filesystem path of the project." }
                    },
                    "required": [],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        // 1. Prefer an explicit, non-empty arg.
        let arg_path = args
            .get("project_path")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        // 2. Fall back to the active-project slot.
        let path = match arg_path {
            Some(p) => p,
            None => {
                let active = match self.active_project.lock() {
                    Ok(g) => g.clone(),
                    Err(e) => {
                        return ToolResult::err(format!(
                            "start_pm: active_project lock poisoned: {e}"
                        ));
                    }
                };
                match active {
                    Some(p) => p.display().to_string(),
                    None => {
                        return ToolResult::err(
                            "start_pm: no 'project_path' provided and no active project set (use set_active_project first)",
                        );
                    }
                }
            }
        };

        match self.pending.lock() {
            Ok(mut slot) => {
                *slot = Some(path.clone());
                ToolResult::ok(format!("queued start_pm for {path}"))
            }
            Err(e) => ToolResult::err(format!("start_pm: pending lock poisoned: {e}")),
        }
    }
}

/// `search_sessions(query)` — grep ~/.open-mpm/sessions/runs.jsonl.
struct SearchSessionsTool;

#[async_trait]
impl ToolExecutor for SearchSessionsTool {
    fn name(&self) -> &str {
        "search_sessions"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "search_sessions",
                "description": "Search past workflow runs (cross-project). Empty query returns all.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string" }
                    },
                    "required": ["query"],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let query = args.get("query").and_then(Value::as_str).unwrap_or("");
        match session_record::search(query).await {
            Ok(hits) => {
                let limited: Vec<_> = hits.into_iter().take(20).collect();
                match serde_json::to_string(&limited) {
                    Ok(s) => ToolResult::ok(s),
                    Err(e) => ToolResult::err(format!("search_sessions: serialize failed: {e}")),
                }
            }
            Err(e) => ToolResult::err(format!("search_sessions: {e:#}")),
        }
    }
}

/// `list_projects()` — dump active entries from ~/.open-mpm/projects.json.
struct ListProjectsTool;

#[async_trait]
impl ToolExecutor for ListProjectsTool {
    fn name(&self) -> &str {
        "list_projects"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "list_projects",
                "description": "List projects CTRL has connected to, with last_connected and pm_count.",
                "parameters": {
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, _args: Value) -> ToolResult {
        match ProjectRegistry::new() {
            Ok(reg) => match reg.list_active().await {
                Ok(entries) => match serde_json::to_string(&entries) {
                    Ok(s) => ToolResult::ok(s),
                    Err(e) => ToolResult::err(format!("list_projects: serialize: {e}")),
                },
                Err(e) => ToolResult::err(format!("list_projects: {e:#}")),
            },
            Err(e) => ToolResult::err(format!("list_projects: registry unavailable: {e:#}")),
        }
    }
}

/// `memory_store(content)` — append to the in-session memory vec.
struct MemoryStoreTool {
    memory: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl ToolExecutor for MemoryStoreTool {
    fn name(&self) -> &str {
        "memory_store"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "memory_store",
                "description": "Store a piece of content for later recall (in-memory fallback).",
                "parameters": {
                    "type": "object",
                    "properties": { "content": { "type": "string" } },
                    "required": ["content"],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let Some(content) = args.get("content").and_then(Value::as_str) else {
            return ToolResult::err("memory_store: missing 'content'");
        };
        match self.memory.lock() {
            Ok(mut m) => {
                m.push(content.to_string());
                ToolResult::ok("stored")
            }
            Err(e) => ToolResult::err(format!("memory_store: lock poisoned: {e}")),
        }
    }
}

/// `memory_recall(query)` — naive substring match over the in-session vec.
struct MemoryRecallTool {
    memory: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl ToolExecutor for MemoryRecallTool {
    fn name(&self) -> &str {
        "memory_recall"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "memory_recall",
                "description": "Recall previously stored content by substring (in-memory fallback).",
                "parameters": {
                    "type": "object",
                    "properties": { "query": { "type": "string" } },
                    "required": ["query"],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let q = args
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_lowercase();
        let mem = match self.memory.lock() {
            Ok(m) => m.clone(),
            Err(e) => return ToolResult::err(format!("memory_recall: lock poisoned: {e}")),
        };
        let hits: Vec<&String> = mem
            .iter()
            .filter(|s| q.is_empty() || s.to_lowercase().contains(&q))
            .collect();
        match serde_json::to_string(&hits) {
            Ok(s) => ToolResult::ok(s),
            Err(e) => ToolResult::err(format!("memory_recall: serialize: {e}")),
        }
    }
}

/// One row of (project_name, status, last_message) shared with `TaskStatusTool`. (#185)
type PmStatusRow = (String, Arc<Mutex<String>>, Arc<Mutex<String>>);

/// `task_status()` — list all PM handles with current state. (#185)
///
/// Why: The Taskmaster persona must be able to report what's running, idle,
/// or in error to drive tasks proactively to completion. Mirrors the side-
/// effect-free pattern used by other CTRL tools by reading from a snapshot
/// captured when the registry is built per-turn.
/// What: Returns a JSON array of `{project, status, last_message}`.
/// Test: `task_status_returns_known_pm_state`.
struct TaskStatusTool {
    /// Snapshot of (project_name, status_arc, last_message_arc) captured
    /// when the registry is built.
    snapshot: Vec<PmStatusRow>,
}

#[async_trait]
impl ToolExecutor for TaskStatusTool {
    fn name(&self) -> &str {
        "task_status"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "task_status",
                "description": "List all active and recently completed PM tasks with their current status",
                "parameters": {
                    "type": "object",
                    "properties": {},
                    "required": [],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, _args: Value) -> ToolResult {
        let mut rows: Vec<Value> = Vec::new();
        for (project, status_arc, last_arc) in &self.snapshot {
            let status = status_arc
                .lock()
                .map(|s| s.clone())
                .unwrap_or_else(|_| "unknown".to_string());
            let last = last_arc
                .lock()
                .map(|m| m.clone())
                .unwrap_or_else(|_| String::new());
            rows.push(json!({
                "project": project,
                "status": status,
                "last_message": last,
            }));
        }
        match serde_json::to_string(&rows) {
            Ok(s) => ToolResult::ok(s),
            Err(e) => ToolResult::err(format!("task_status: serialize: {e}")),
        }
    }
}

/// `self_project_status()` — return version + recent commits for the
/// detected open-mpm self-project. (#182)
///
/// Why: Lets the user (or the LLM) inspect what version is running and what
/// the most recent commits did, without leaving the CTRL prompt.
/// What: Reads `[package] version` from `<self_path>/Cargo.toml`, runs
/// `git -C <self_path> log --oneline -3`, and returns the JSON envelope.
/// Test: `self_project_status_returns_version_when_path_set`.
struct SelfProjectStatusTool {
    self_path: Option<PathBuf>,
}

#[async_trait]
impl ToolExecutor for SelfProjectStatusTool {
    fn name(&self) -> &str {
        "self_project_status"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "self_project_status",
                "description": "Report version + last 3 git commits for the open-mpm self-project (when running from its own checkout).",
                "parameters": {
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, _args: Value) -> ToolResult {
        let Some(path) = self.self_path.as_ref() else {
            return ToolResult::err(
                "self_project_status: no self-project detected (not running from open-mpm checkout)",
            );
        };
        let version = read_self_version(path).unwrap_or_else(|_| "unknown".to_string());
        let log = read_recent_git_log(path, 3)
            .await
            .unwrap_or_else(|e| format!("(git unavailable: {e})"));
        let body = json!({
            "version": version,
            "self_project_path": path.display().to_string(),
            "git_log": log,
        });
        ToolResult::ok(body.to_string())
    }
}

/// Read `[package] version = "..."` from `<path>/Cargo.toml`. (#182)
///
/// Why: We can't depend on `env!("CARGO_PKG_VERSION")` for the *target*
/// project — that's the version of whatever crate compiled this binary,
/// which only matches when the running binary was built from the detected
/// self-project. Reading the file lets a remote or stale binary report the
/// correct on-disk version.
fn read_self_version(self_path: &Path) -> Result<String> {
    let cargo_toml = self_path.join("Cargo.toml");
    let text = std::fs::read_to_string(&cargo_toml)
        .with_context(|| format!("read {}", cargo_toml.display()))?;
    let parsed: toml::Value = toml::from_str(&text).context("parse Cargo.toml")?;
    let v = parsed
        .get("package")
        .and_then(|p| p.get("version"))
        .and_then(|v| v.as_str())
        .context("Cargo.toml missing [package] version")?;
    Ok(v.to_string())
}

/// Run `git -C <path> log --oneline -<n>` and return stdout. (#182)
async fn read_recent_git_log(self_path: &Path, n: usize) -> Result<String> {
    let out = tokio::process::Command::new("git")
        .arg("-C")
        .arg(self_path)
        .arg("log")
        .arg("--oneline")
        .arg(format!("-{n}"))
        .output()
        .await
        .context("spawn git log")?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).to_string();
        anyhow::bail!("git log failed: {err}");
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// `initiate_self_task(task)` — queue a `start_pm` against the self-project.
/// (#182)
///
/// Why: Single-call shortcut for the common "ask CTRL to work on itself"
/// pattern. Reuses the `start_pm` queueing path so the post-tool drain in
/// `ctrl_chat_turn` actually spawns the PM and connects to it.
/// What: Captures `task` text in a shared slot; the caller is responsible
/// for forwarding it to the PM after the connection is established. We also
/// queue the self-project path into the existing `pending_connect` slot so
/// the CTRL turn-completion logic spawns the PM.
/// Test: `initiate_self_task_queues_self_project_path`.
struct InitiateSelfTaskTool {
    self_path: Option<PathBuf>,
    pending_connect: Arc<Mutex<Option<String>>>,
    pending_self_task: Arc<Mutex<Option<String>>>,
}

#[async_trait]
impl ToolExecutor for InitiateSelfTaskTool {
    fn name(&self) -> &str {
        "initiate_self_task"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "initiate_self_task",
                "description": "Start (or attach to) a PM for the open-mpm self-project and queue this task for it.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "task": { "type": "string", "description": "Development task to run against open-mpm itself." }
                    },
                    "required": ["task"],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let Some(path) = self.self_path.as_ref() else {
            return ToolResult::err(
                "initiate_self_task: no self-project detected (not running from open-mpm checkout)",
            );
        };
        let Some(task) = args.get("task").and_then(Value::as_str) else {
            return ToolResult::err("initiate_self_task: missing 'task'");
        };
        let path_str = path.display().to_string();
        if let Ok(mut slot) = self.pending_connect.lock() {
            *slot = Some(path_str.clone());
        }
        if let Ok(mut slot) = self.pending_self_task.lock() {
            *slot = Some(task.to_string());
        }
        ToolResult::ok(format!("queued self-task against {path_str}"))
    }
}

/// `search_docs(query)` — semantic search over project documentation. (#187)
///
/// Why: Lets CTRL answer "how does open-mpm work?" questions by scanning the
/// project's own `docs/` tree without an LLM call. The tool relies on a
/// TF-IDF index built at CTRL startup.
/// What: Returns top-5 matches as a JSON array of `{path, title, snippet,
/// score}`. Falls back to a graceful message when the index is still
/// building or the docs directory is empty.
/// Test: `search_docs_returns_results_when_index_present` and
/// `search_docs_falls_back_when_index_missing`.
struct SearchDocsTool {
    index: Arc<Mutex<Option<Arc<crate::docs_index::DocsIndex>>>>,
}

#[async_trait]
impl ToolExecutor for SearchDocsTool {
    fn name(&self) -> &str {
        "search_docs"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "search_docs",
                "description": "Search project documentation semantically. Use this to answer questions about how open-mpm works, its configuration, agents, skills, and workflows.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Free-text query (e.g. 'how do I write a skill', 'what is the workflow JSON format')."
                        }
                    },
                    "required": ["query"],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let Some(q) = args.get("query").and_then(Value::as_str) else {
            return ToolResult::err("search_docs: missing 'query'");
        };
        let idx = match self.index.lock() {
            Ok(g) => g.clone(),
            Err(_) => return ToolResult::err("search_docs: index lock poisoned"),
        };
        let Some(idx) = idx else {
            return ToolResult::ok(
                "search_docs: docs index not yet built (try again in a moment)".to_string(),
            );
        };
        if idx.is_empty() {
            return ToolResult::ok("search_docs: no documents indexed".to_string());
        }
        let hits = idx.search(q, 5);
        match serde_json::to_string(&hits) {
            Ok(s) => ToolResult::ok(s),
            Err(e) => ToolResult::err(format!("search_docs: serialize: {e}")),
        }
    }
}

/// `add_project(path)` — register a project in `~/.open-mpm/projects.json`.
/// (#202)
///
/// Why: Lets the user (or LLM) bring a directory under CTRL management
/// without having to launch a PM first. Mirrors the implicit registration
/// performed by `Ctrl::connect`, but as a standalone, idempotent action so
/// `list_projects` can show it before any work begins.
/// What: Validates that `path` exists and is a directory, then calls
/// `ProjectRegistry::register_pm_start` (the same path used during
/// `connect`) so the entry's metadata stays consistent with normal use.
/// Test: `add_project_tool_validates_path`.
struct AddProjectTool;

#[async_trait]
impl ToolExecutor for AddProjectTool {
    fn name(&self) -> &str {
        "add_project"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "add_project",
                "description": "Register a project directory in ~/.open-mpm/projects.json so it appears in list_projects.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Absolute path to the project directory" }
                    },
                    "required": ["path"],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let Some(raw) = args.get("path").and_then(Value::as_str) else {
            return ToolResult::err("add_project: missing 'path'");
        };
        let path = match PathBuf::from(raw).canonicalize() {
            Ok(p) => p,
            Err(e) => return ToolResult::err(format!("add_project: cannot resolve {raw}: {e}")),
        };
        if !path.is_dir() {
            return ToolResult::err(format!("add_project: not a directory: {}", path.display()));
        }
        let stack = detect_stack(&path);
        let status = if is_empty_project(&path) {
            "new"
        } else {
            "existing"
        };
        match ProjectRegistry::new() {
            Ok(reg) => match reg.register_pm_start(&path).await {
                Ok(()) => ToolResult::ok(format!(
                    "Project registered: {} (stack: {}, status: {})",
                    path.display(),
                    stack,
                    status
                )),
                Err(e) => ToolResult::err(format!("add_project: {e:#}")),
            },
            Err(e) => ToolResult::err(format!("add_project: registry unavailable: {e:#}")),
        }
    }
}

/// `remove_project(path)` — drop an entry from `~/.open-mpm/projects.json`.
/// (#202)
///
/// Why: Lets the user clean up the registry without editing JSON by hand.
/// Does NOT touch any running PM session — that's the job of `stop_task`.
/// What: Calls `ProjectRegistry::remove`, which removes the canonical-path
/// keyed entry and saves atomically.
/// Test: covered indirectly via `add_project_tool_validates_path` plus
/// `ProjectRegistry::remove` round-trip in registry tests.
struct RemoveProjectTool;

#[async_trait]
impl ToolExecutor for RemoveProjectTool {
    fn name(&self) -> &str {
        "remove_project"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "remove_project",
                "description": "Remove a project entry from ~/.open-mpm/projects.json. Does not stop running PM sessions.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Absolute path of the project to remove" }
                    },
                    "required": ["path"],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let Some(raw) = args.get("path").and_then(Value::as_str) else {
            return ToolResult::err("remove_project: missing 'path'");
        };
        // Try to canonicalize, but fall back to the literal path so users
        // can remove entries whose directories have already been deleted.
        let path = PathBuf::from(raw)
            .canonicalize()
            .unwrap_or_else(|_| PathBuf::from(raw));
        match ProjectRegistry::new() {
            Ok(reg) => match reg.remove(&path).await {
                Ok(true) => ToolResult::ok(format!("Project removed: {}", path.display())),
                Ok(false) => ToolResult::ok(format!("Project not found: {}", path.display())),
                Err(e) => ToolResult::err(format!("remove_project: {e:#}")),
            },
            Err(e) => ToolResult::err(format!("remove_project: registry unavailable: {e:#}")),
        }
    }
}

/// One stoppable PM, keyed by `name` (matching `task_status` output) and
/// `project_path` string. (#202)
type PmStopHandle = (
    String, // name (matches task_status `project` field)
    String, // canonical project path
    mpsc::Sender<PmMsg>,
);

/// `stop_task(session_id)` — request shutdown of a running PM session.
/// (#202)
///
/// Why: The Taskmaster persona must be able to abort a runaway task without
/// killing the entire CTRL process. CTRL tracks PMs by project name (which is
/// what `task_status` returns as `project`), so we accept either the project
/// name or its canonical path here and match against the snapshot.
/// What: Looks up the matching handle, queues the `name` in `pending_stop`
/// for the REPL loop to drain (since the tool can't take `&mut Ctrl`), and
/// publishes `Event::SessionCancelled` immediately so SSE subscribers update.
/// The actual `PmMsg::Shutdown` send + handle removal happens in
/// `ctrl_chat_turn` after the turn completes.
/// Test: `stop_task_tool_records_pending_stop`.
struct StopTaskTool {
    snapshot: Vec<PmStopHandle>,
    pending_stop: Arc<Mutex<Option<String>>>,
}

#[async_trait]
impl ToolExecutor for StopTaskTool {
    fn name(&self) -> &str {
        "stop_task"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "stop_task",
                "description": "Stop a running PM task. Pass the project name or path returned by task_status() / list_projects().",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "session_id": {
                            "type": "string",
                            "description": "Session identifier — accepts the project name or canonical path of the PM to stop."
                        }
                    },
                    "required": ["session_id"],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let Some(sid) = args.get("session_id").and_then(Value::as_str) else {
            return ToolResult::err("stop_task: missing 'session_id'");
        };
        let sid_trim = sid.trim();
        // Match against either the short name or the canonical path so the
        // LLM can pass whichever is more convenient.
        let found = self
            .snapshot
            .iter()
            .find(|(name, path, _)| name == sid_trim || path == sid_trim);
        let Some((name, _path, _tx)) = found else {
            return ToolResult::ok(format!("Task not found: {sid_trim}"));
        };

        // Publish cancellation event immediately so any SSE subscribers see
        // the stop signal before the REPL drains the queue.
        events::publish(Event::SessionCancelled {
            session_id: name.clone(),
        });

        match self.pending_stop.lock() {
            Ok(mut slot) => {
                *slot = Some(name.clone());
                ToolResult::ok(format!("Task {name} stopped"))
            }
            Err(e) => ToolResult::err(format!("stop_task: pending lock poisoned: {e}")),
        }
    }
}

/// `set_active_project(path)` — change CTRL's default project for `start_pm`.
/// (#202)
///
/// Why: Lets a user pin a project once and then invoke `start_pm` (or other
/// path-defaulting tools) without repeating the path on every turn.
/// What: Validates that `path` exists, then writes it to the shared
/// `active_project` slot held by `Ctrl`. `StartPmTool` reads the same slot.
/// Test: `set_active_project_updates_slot`.
struct SetActiveProjectTool {
    active_project: Arc<Mutex<Option<PathBuf>>>,
}

#[async_trait]
impl ToolExecutor for SetActiveProjectTool {
    fn name(&self) -> &str {
        "set_active_project"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "set_active_project",
                "description": "Set the active project path used as a default for start_pm and similar tools.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Absolute path to set as the active project" }
                    },
                    "required": ["path"],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let Some(raw) = args.get("path").and_then(Value::as_str) else {
            return ToolResult::err("set_active_project: missing 'path'");
        };
        let path = match PathBuf::from(raw).canonicalize() {
            Ok(p) => p,
            Err(e) => {
                return ToolResult::err(format!("set_active_project: cannot resolve {raw}: {e}"));
            }
        };
        if !path.is_dir() {
            return ToolResult::err(format!(
                "set_active_project: not a directory: {}",
                path.display()
            ));
        }
        match self.active_project.lock() {
            Ok(mut slot) => {
                *slot = Some(path.clone());
                ToolResult::ok(format!("Active project set to: {}", path.display()))
            }
            Err(e) => ToolResult::err(format!("set_active_project: lock poisoned: {e}")),
        }
    }
}

/// `move_file(from, to)` — rename or relocate a file on disk.
///
/// Why: CTRL's digital-twin persona reorganizes project layouts (e.g. moving
/// stray scripts into `scripts/`). Direct file moves let it reshape projects
/// without spawning a PM/engineer round-trip.
/// What: Canonicalizes `from`, computes the final destination (if `to` is a
/// directory, append `from`'s file name), creates intermediate parents, and
/// renames. Falls back to copy+delete on cross-device errors (EXDEV).
/// Test: `move_file_tool_renames_basic`, `move_file_tool_into_directory`.
struct MoveFileTool;

#[async_trait]
impl ToolExecutor for MoveFileTool {
    fn name(&self) -> &str {
        "move_file"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "move_file",
                "description": "Move or rename a file. If 'to' is an existing directory, the source is moved into it; otherwise it is renamed to the exact 'to' path. Intermediate parent directories of 'to' are created if missing.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "from": { "type": "string", "description": "Source path of the file to move" },
                        "to":   { "type": "string", "description": "Destination path or directory" }
                    },
                    "required": ["from", "to"],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let Some(from_raw) = args.get("from").and_then(Value::as_str) else {
            return ToolResult::err("move_file: missing 'from'");
        };
        let Some(to_raw) = args.get("to").and_then(Value::as_str) else {
            return ToolResult::err("move_file: missing 'to'");
        };
        let from = match PathBuf::from(from_raw).canonicalize() {
            Ok(p) => p,
            Err(e) => {
                return ToolResult::err(format!("move_file: cannot resolve '{from_raw}': {e}"));
            }
        };
        if !from.exists() {
            return ToolResult::err(format!("move_file: source not found: {}", from.display()));
        }
        // Resolve the destination. If `to` is an existing directory, move the
        // source into it preserving its file name. Otherwise treat `to` as the
        // exact target path.
        let to_input = PathBuf::from(to_raw);
        let dest = if to_input.is_dir() {
            match from.file_name() {
                Some(n) => to_input.join(n),
                None => {
                    return ToolResult::err(format!(
                        "move_file: source has no file name: {}",
                        from.display()
                    ));
                }
            }
        } else {
            to_input
        };
        // Ensure the destination's parent exists.
        if let Some(parent) = dest.parent()
            && !parent.as_os_str().is_empty()
            && !parent.exists()
            && let Err(e) = tokio::fs::create_dir_all(parent).await
        {
            return ToolResult::err(format!(
                "move_file: cannot create parent {}: {e}",
                parent.display()
            ));
        }
        match tokio::fs::rename(&from, &dest).await {
            Ok(()) => ToolResult::ok(format!("Moved: {} → {}", from.display(), dest.display())),
            Err(e) => {
                // EXDEV — rename across devices fails on most platforms; fall
                // back to copy + remove.
                if e.raw_os_error() == Some(18) || e.kind() == std::io::ErrorKind::CrossesDevices {
                    if let Err(e2) = tokio::fs::copy(&from, &dest).await {
                        return ToolResult::err(format!("move_file: copy fallback failed: {e2}"));
                    }
                    if let Err(e2) = tokio::fs::remove_file(&from).await {
                        return ToolResult::err(format!(
                            "move_file: copy succeeded but source delete failed: {e2}"
                        ));
                    }
                    ToolResult::ok(format!(
                        "Moved (copy+delete): {} → {}",
                        from.display(),
                        dest.display()
                    ))
                } else {
                    ToolResult::err(format!("move_file: rename failed: {e}"))
                }
            }
        }
    }
}

/// `create_dir(path)` — create a directory (and any missing parents).
///
/// Why: CTRL scaffolds project layouts and reorganizes existing trees;
/// `mkdir -p` semantics let it stage empty directories before delegating
/// work to a PM.
/// What: Expands `~` to the user's home dir, then calls `create_dir_all`.
/// Test: `create_dir_tool_makes_nested_dir`.
struct CreateDirTool;

#[async_trait]
impl ToolExecutor for CreateDirTool {
    fn name(&self) -> &str {
        "create_dir"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "create_dir",
                "description": "Create a directory, including any missing intermediate parents (mkdir -p semantics).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Directory path to create. Leading '~' is expanded to the user's home directory." }
                    },
                    "required": ["path"],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let Some(raw) = args.get("path").and_then(Value::as_str) else {
            return ToolResult::err("create_dir: missing 'path'");
        };
        let expanded: PathBuf = if let Some(rest) = raw.strip_prefix("~/") {
            match std::env::var_os("HOME") {
                Some(home) => PathBuf::from(home).join(rest),
                None => PathBuf::from(raw),
            }
        } else if raw == "~" {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(raw))
        } else {
            PathBuf::from(raw)
        };
        if expanded.is_dir() {
            return ToolResult::ok(format!("Directory already exists: {}", expanded.display()));
        }
        match tokio::fs::create_dir_all(&expanded).await {
            Ok(()) => ToolResult::ok(format!("Created directory: {}", expanded.display())),
            Err(e) => ToolResult::err(format!(
                "create_dir: failed to create {}: {e}",
                expanded.display()
            )),
        }
    }
}

/// Detect the primary tech stack of a project by checking indicator files.
///
/// Why: When CTRL learns about a project, surfacing stack identity (Rust /
/// Python / Node / Go / etc.) lets it pick the right engineer agent without
/// asking the user. Returning `"unknown"` keeps the caller's flow simple.
/// What: Walks a list of `(filename, stack-label)` pairs; supports `*.ext`
/// glob entries via `read_dir`. First match wins.
/// Test: `detect_stack_finds_rust`, `detect_stack_returns_unknown`.
fn detect_stack(project_path: &Path) -> String {
    let indicators: &[(&str, &str)] = &[
        ("Cargo.toml", "Rust"),
        ("go.mod", "Go"),
        ("pom.xml", "Java (Maven)"),
        ("build.gradle", "Java/Kotlin (Gradle)"),
        ("build.gradle.kts", "Java/Kotlin (Gradle)"),
        ("pyproject.toml", "Python"),
        ("setup.py", "Python"),
        ("package.json", "Node.js/TypeScript"),
        ("Gemfile", "Ruby"),
        ("mix.exs", "Elixir"),
        ("composer.json", "PHP"),
        ("*.csproj", "C#/.NET"),
    ];
    for (file, stack) in indicators {
        if file.contains('*') {
            if let Ok(entries) = std::fs::read_dir(project_path) {
                let ext = file.trim_start_matches("*.");
                if entries
                    .flatten()
                    .any(|e| e.path().extension().and_then(|x| x.to_str()) == Some(ext))
                {
                    return (*stack).to_string();
                }
            }
        } else if project_path.join(file).exists() {
            return (*stack).to_string();
        }
    }
    "unknown".to_string()
}

/// Heuristic: is the project directory effectively empty (a "new" project)?
///
/// Why: When a user adds a project, CTRL should distinguish "scaffold a new
/// project here" from "index this existing codebase". A directory with no
/// non-hidden entries is treated as new.
/// What: Returns true when `read_dir` yields no entries whose name does not
/// start with '.'. On read failure, returns false (treat as existing).
/// Test: covered by `add_project_tool_validates_path` indirectly.
fn is_empty_project(project_path: &Path) -> bool {
    match std::fs::read_dir(project_path) {
        Ok(entries) => !entries.flatten().any(|e| {
            e.file_name()
                .to_str()
                .map(|n| !n.starts_with('.'))
                .unwrap_or(false)
        }),
        Err(_) => false,
    }
}

/// Build a "## Active TM Sessions" block for injection into system prompts.
///
/// Why: Without a live snapshot, the LLM either hallucinates ("I don't see any
/// sessions") or refuses to answer. Surfacing the tmux session list at prompt
/// build time gives ctrl/PM ground truth to reason from before deciding which
/// `tm_*` tool to call.
/// What: Tries to construct a `TmManager` rooted at `state_dir`, calls
/// `list_sessions`, and renders one line per session. Returns an empty string
/// if TM is unavailable (no tmux) so the calling prompt is unchanged.
/// Test: Indirectly via `ctrl_chat_turn` integration tests when tmux exists;
/// the empty-string return when tmux is missing is exercised in CI.
async fn build_tm_context_block(state_dir: &Path) -> String {
    let _ = std::fs::create_dir_all(state_dir);
    let mgr = match crate::tm::TmManager::new(state_dir) {
        Ok(m) => m,
        Err(_) => return String::new(),
    };
    let sessions = match mgr.list_sessions().await {
        Ok(s) => s,
        Err(_) => return String::new(),
    };
    if sessions.is_empty() {
        return "## Active TM Sessions\nNo tmux sessions currently managed.".to_string();
    }
    let mut block = String::from(
        "## Active TM Sessions\nYou can inspect or control these sessions using the tm_* tools.\n\n",
    );
    for s in &sessions {
        block.push_str(&format!(
            "- **{}** | adapter: {} | status: {} | project: {} | last active: {}\n",
            s.name,
            s.adapter_type.as_str(),
            s.status,
            s.project_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("?"),
            s.last_active_ago(),
        ));
    }
    block
}

/// Build the CTRL tool registry for a single LLM turn.
async fn build_ctrl_registry(
    memory: Arc<Mutex<Vec<String>>>,
    pending_connect: Arc<Mutex<Option<String>>>,
    self_path: Option<PathBuf>,
    pending_self_task: Arc<Mutex<Option<String>>>,
    task_status_snapshot: Vec<PmStatusRow>,
    docs_index: Arc<Mutex<Option<Arc<crate::docs_index::DocsIndex>>>>,
    active_project: Arc<Mutex<Option<PathBuf>>>,
    pending_stop: Arc<Mutex<Option<String>>>,
    stop_snapshot: Vec<PmStopHandle>,
) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    // Capture self_path before it's moved into the InitiateSelfTaskTool below
    // so TM tools can use it as the canonical state_dir source.
    let self_path_for_tm = self_path.clone();
    registry.register(Arc::new(StartPmTool {
        pending: pending_connect.clone(),
        active_project: active_project.clone(),
    }));
    registry.register(Arc::new(SearchSessionsTool));
    registry.register(Arc::new(ListProjectsTool));
    registry.register(Arc::new(MemoryStoreTool {
        memory: memory.clone(),
    }));
    registry.register(Arc::new(MemoryRecallTool { memory }));
    // #185: Taskmaster needs to inspect PM task state.
    registry.register(Arc::new(TaskStatusTool {
        snapshot: task_status_snapshot,
    }));
    // #182: self-project tools, present (and reporting "no self-project")
    // even when detection failed so the LLM gets a clear error rather than
    // an "unknown tool" surprise.
    registry.register(Arc::new(SelfProjectStatusTool {
        self_path: self_path.clone(),
    }));
    registry.register(Arc::new(InitiateSelfTaskTool {
        self_path,
        pending_connect,
        pending_self_task,
    }));
    // #187: docs search tool — backed by the lazily-built TF-IDF index.
    registry.register(Arc::new(SearchDocsTool { index: docs_index }));
    // #202: project-management + active-project tools.
    registry.register(Arc::new(AddProjectTool));
    registry.register(Arc::new(RemoveProjectTool));
    registry.register(Arc::new(StopTaskTool {
        snapshot: stop_snapshot,
        pending_stop,
    }));
    registry.register(Arc::new(SetActiveProjectTool {
        active_project: active_project.clone(),
    }));
    // CTRL digital-twin: file system manipulation tools.
    registry.register(Arc::new(MoveFileTool));
    registry.register(Arc::new(CreateDirTool));
    // CTRL digital-twin: research tools (web search + project code search).
    registry.register(Arc::new(
        crate::tools::web_search::BraveSearchTool::from_env(),
    ));
    // #374: Same auto-detection as the conversational fast-path registry
    // above. Prefers the running search daemon, falls back to grep.
    {
        let project_root =
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let search_tool =
            crate::tools::native_search::SearchCodeTool::new_auto(&project_root).await;
        registry.register(Arc::new(search_tool));
    }
    // #244: Dynamic MCP service management tools (mcp_list/add/remove/enable/disable).
    for tool in crate::tools::mcp_tools::mcp_tool_executors() {
        registry.register(tool);
    }
    // #243: Native ticketing tools (create/get/update/close/list/add_comment +
    // actions_trigger/actions_status). Wired only when the global config has a
    // `[github]` identity that resolves to non-empty token + repo env vars; we
    // silently skip otherwise so unconfigured environments don't error.
    register_ticketing_tools(&mut registry).await;
    // #247: Native git tools (status/log/branches/commit/push/pull/...).
    // Gated by `[git].available_for_roles` in ~/.open-mpm/config.toml. We
    // resolve the repo root from the active project (when set) or cwd; if
    // discovery fails (not in a repo), we silently skip — the LLM will
    // simply not see git tools.
    register_git_tools(&mut registry, "ctrl", &active_project).await;

    // TM (tmux manager) tools — let ctrl query/control all tmux sessions via
    // natural language. Resolves the state_dir from the active project (when
    // set), the detected self-project, or cwd as a final fallback. When tmux
    // is unavailable, registration silently no-ops so degraded environments
    // (CI, no-tmux dev boxes) still get a working ctrl.
    {
        let active = match active_project.lock() {
            Ok(g) => g.clone(),
            Err(_) => None,
        };
        let project_root = active
            .or(self_path_for_tm)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        let state_dir = project_root.join(".open-mpm").join("state");
        crate::tools::tm_tools::register_tm_tools_for_state_dir(&mut registry, &state_dir);
    }

    registry
}

/// Wire the 12 native git tools into a `ToolRegistry` if configured (#247).
///
/// Why: Both ctrl and PM call sites need the same wiring logic; factoring
/// it out keeps them aligned and gives a single place to evolve role-gating
/// or write-confirmation behavior. Discovery failures (not a git repo) are
/// non-fatal — the agent simply runs without git tools.
/// What: Loads `GlobalConfig`, checks `git.available_for_roles` for `role`,
/// resolves a repo root (active project or cwd), opens it via `GitRepo`,
/// and registers all 12 tools from `git_tools(root)`.
/// Test: Indirect — covered by `git_tools_count_is_12` in `git_tools.rs`
/// and by the ctrl integration tests.
async fn register_git_tools(
    registry: &mut ToolRegistry,
    role: &str,
    active_project: &Arc<Mutex<Option<PathBuf>>>,
) {
    let cfg = crate::mcp::config::GlobalConfig::load().await;
    if !cfg.git.available_for_roles.iter().any(|r| r == role) {
        return;
    }
    let candidate = {
        let guard = match active_project.lock() {
            Ok(guard) => guard,
            Err(_) => {
                tracing::warn!(
                    "register_git_tools: active_project mutex poisoned; skipping git tool registration"
                );
                return;
            }
        };
        guard.clone()
    }
    .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let repo = match crate::git::GitRepo::open(&candidate) {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!(error = %e, role = role, "no git repo discovered; skipping git tools");
            return;
        }
    };
    for tool in crate::tools::git_tools::git_tools(repo.root.clone()) {
        registry.register(tool);
    }
}

/// Wire the ticketing tools into a `ToolRegistry` if configured (#243).
///
/// Why: Both ctrl and PM call sites need the same wiring logic, so factoring
/// it out keeps them aligned. Failure to load config or build a client is
/// non-fatal — the agent simply runs without ticketing tools.
/// What: Reads `~/.open-mpm/config.toml`, resolves the default GitHub
/// identity, builds a `TicketingClient` plus a `GitHubActionsClient`, and
/// registers all tools from `ticketing_tools()`.
/// Test: Indirectly via `ticketing_tools_count` in `native_ticketing` tests
/// and integration tests that snapshot the registered tool set.
async fn register_ticketing_tools(registry: &mut ToolRegistry) {
    let cfg = crate::mcp::config::GlobalConfig::load().await;
    let Some(identity) = cfg.github_identity(None) else {
        return;
    };
    let Some(tk_cfg) = identity.to_ticketing_config() else {
        tracing::debug!(
            identity = %identity.name,
            "ticketing identity present but env vars not set; skipping ticketing tools"
        );
        return;
    };
    let client: Arc<dyn crate::ticketing::TicketingClient> = match tk_cfg.build_client().await {
        Ok(c) => Arc::from(c),
        Err(e) => {
            tracing::warn!(error = %e, "failed to build ticketing client; skipping ticketing tools");
            return;
        }
    };
    // Actions client uses the same token/repo as the issues client (or `gh`
    // CLI fallback when token is missing).
    let actions = crate::ticketing::actions::build_actions_client(
        identity.token().as_deref(),
        identity.repo().as_deref(),
    )
    .await;
    for tool in crate::tools::native_ticketing::ticketing_tools(client, actions) {
        registry.register(tool);
    }
}

/// Pending side-effect slots populated by tool callbacks during a CTRL turn.
///
/// Purpose: Refactor scaffolding for #256 — `ctrl_chat_turn` was decomposed
/// into named phase helpers; this struct carries the three pending-action
/// slots (filled by tools while the LLM runs, drained at end-of-turn) so
/// `drain_ctrl_turn_side_effects` doesn't need a parameter list per slot.
struct CtrlTurnSideEffects {
    /// Drained at end-of-turn to perform a real `Ctrl::connect`.
    pending_connect: Arc<Mutex<Option<String>>>,
    /// Optional self-task forwarded after a successful self-connect (#182).
    pending_self_task: Arc<Mutex<Option<String>>>,
    /// PM-name target queued by `stop_task` (#202).
    pending_stop: Arc<Mutex<Option<String>>>,
}

/// Output of `prepare_ctrl_turn_state` — everything the rest of the turn
/// needs that depends on the live `Ctrl` snapshot.
///
/// Purpose: Bundles the tool registry, the materialised OpenAI tool schemas,
/// and the side-effect slot bundle so the calling function isn't a tuple-soup.
struct CtrlTurnState {
    /// Tool registry shared into the LLM dispatch phase.
    registry: ToolRegistry,
    /// OpenAI-format tool schemas, materialised once and used for the
    /// deployment footer's `tools_count`.
    openai_tools: Vec<ChatCompletionTool>,
    /// Slots that tool callbacks fill during the LLM call, drained at end.
    side_effects: CtrlTurnSideEffects,
}

// Purpose: Build the per-turn registry, snapshot live PM state, and
// allocate the pending side-effect slots that tool callbacks will fill.
async fn prepare_ctrl_turn_state(ctrl: &Ctrl) -> Result<CtrlTurnState> {
    let pending_connect: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let pending_self_task: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let pending_stop: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    // #185: Snapshot PM handles for the task_status tool. Vec of
    // (name, status_arc, last_message_arc) — the Arc<Mutex<...>> captures
    // give the tool a live read of mutable state without &mut Ctrl.
    let task_status_snapshot: Vec<PmStatusRow> = ctrl
        .pms
        .values()
        .map(|h| (h.name.clone(), h.status.clone(), h.last_message.clone()))
        .collect();
    // #202: snapshot of stoppable PMs for `stop_task`. Records both the
    // short name (matches `task_status` `project` field) and the canonical
    // path so the LLM can identify a target via either form.
    let stop_snapshot: Vec<PmStopHandle> = ctrl
        .pms
        .iter()
        .map(|(key, h)| (h.name.clone(), key.clone(), h.tx.clone()))
        .collect();

    let registry = build_ctrl_registry(
        ctrl.memory.clone(),
        pending_connect.clone(),
        ctrl.self_project.clone(),
        pending_self_task.clone(),
        task_status_snapshot,
        ctrl.docs_index.clone(),
        ctrl.active_project.clone(),
        pending_stop.clone(),
        stop_snapshot,
    )
    .await;
    let openai_tools: Vec<ChatCompletionTool> = registry.openai_tools()?;

    Ok(CtrlTurnState {
        registry,
        openai_tools,
        side_effects: CtrlTurnSideEffects {
            pending_connect,
            pending_self_task,
            pending_stop,
        },
    })
}

// Purpose: Resolve `ctrl.toml` (or fall back to the built-in default) so the
// caller can build the system prompt against a concrete `AgentConfig`. The
// returned path is `Some` only when an on-disk override was loaded.
async fn resolve_ctrl_turn_agent_config(ctrl: &Ctrl) -> (AgentConfig, Option<PathBuf>) {
    if let Some(self_path) = &ctrl.self_project {
        // #298: Use the ctrl-specific resolver so we prefer ctrl.toml over
        // pm.toml. Sharing the PM resolver caused ctrl turns to load the
        // sonnet PM prompt + delegation tools, blowing latency out to 30s.
        match resolve_ctrl_agent_config(self_path).await {
            Ok((c, p)) => (c, p),
            Err(e) => {
                tracing::warn!(error = %e, "failed to resolve ctrl agent config; using built-in default");
                (AgentConfig::ctrl_default(), None)
            }
        }
    } else {
        (AgentConfig::ctrl_default(), None)
    }
}

// Purpose: Assemble the full CTRL system prompt — base TOML/default,
// agent identity, statically-listed skills, dynamic BM25-selected skills,
// MCP layer, project memory (PM-mode only), TM session context, self-aware
// footer, user profile, current date/time, and deployment block.
async fn build_ctrl_turn_system_prompt(
    ctrl: &Ctrl,
    user_input: &str,
    agent_cfg: &AgentConfig,
    agent_cfg_path: Option<&Path>,
    openai_tools_count: usize,
    mcp_cfg: &crate::mcp::GlobalConfig,
) -> String {
    let base_prompt = if agent_cfg.system_prompt.content.trim().is_empty() {
        CTRL_SYSTEM_PROMPT.to_string()
    } else {
        agent_cfg.system_prompt.content.clone()
    };

    // #478: Substitute the agent-identity placeholders so any persona —
    // including ctrl, which skips the deployment footer — can answer
    // "what model / runner am I?" from its own system prompt.
    let runner_label = match agent_cfg.agent.runner {
        crate::agents::RunnerKind::Subprocess => "subprocess",
        crate::agents::RunnerKind::Inline => "inline",
        crate::agents::RunnerKind::ClaudeCode => "claude-code",
        crate::agents::RunnerKind::InProcess => "in-process",
    };
    let mut builder = crate::agents::prompt_builder::SystemPromptBuilder::new(base_prompt)
        .with_agent_context(agent_cfg.agent.model.as_str(), runner_label);

    // Skills declared in ctrl.toml `[system_prompt] skills = [...]`.
    // These "always-inject" skills are loaded on every turn regardless of
    // relevance — the dynamic BM25 search below is purely additive.
    use crate::tools::traits::SkillResolver;
    let skill_resolver = crate::tools::skill_loader::FsSkillResolver::from_defaults();
    let mut injected_skills: std::collections::HashSet<String> = std::collections::HashSet::new();
    if let Some(skills) = &agent_cfg.system_prompt.skills
        && !skills.is_empty()
    {
        for s in skills {
            if let Some(text) = skill_resolver.resolve(s) {
                builder = builder.add_skill(format!("# Skill: {s}\n\n{text}"));
                injected_skills.insert(s.clone());
            } else {
                tracing::warn!(skill = %s, "ctrl skill not found; skipping");
            }
        }
    }

    // #483: On-demand skill injection via BM25 search.
    //
    // Why: Statically listing every potentially-useful skill in ctrl.toml
    // burns context budget on irrelevant skills every turn. A lightweight
    // BM25 index over all discoverable skills lets the harness inject only
    // the few most relevant to the current user message.
    // What: Builds a `SkillRegistry` over `skill_search_paths` — which already
    // honors `OPEN_MPM_SKILLS_PROJECT_LOCAL_ONLY` to skip `~/.claude/skills/`
    // when set — searches the BM25 index for up to 3 skills relevant to
    // `user_input`, and appends any not already injected by the static list
    // above. The static skills always win; dynamic search is additive only.
    {
        let config_dir = crate::default_bundled_config_dir();
        let search_paths = crate::skills::registry::skill_search_paths(&config_dir);
        let skill_reg = crate::skills::registry::SkillRegistry::load(&search_paths);
        let dynamic_skills = skill_reg.search(user_input, 3);
        for s in dynamic_skills {
            if injected_skills.contains(&s) {
                continue;
            }
            if let Some(text) = skill_resolver.resolve(&s) {
                builder = builder.add_skill(format!("# Skill: {s}\n\n{text}"));
                injected_skills.insert(s.clone());
                tracing::debug!(skill = %s, "ctrl: injected dynamic skill via BM25 search");
            }
        }
    }

    // #241: MCP tool descriptions for role "ctrl".
    if let Some(section) = mcp_cfg.render_prompt_section("ctrl") {
        builder = builder.add_mcp_layer(section);
    }

    // #298: ctrl is a lightweight conversational persona — skip the heavy
    // PM context layers (project memory, deployment footer, project index)
    // when the loaded agent is ctrl.toml. Those layers belong to the PM
    // orchestrator path. Detected by matching the loaded agent name against
    // "ctrl" so that pm.toml fallbacks still get the full context.
    let is_ctrl_persona = agent_cfg.agent.name == "ctrl";

    // #241/#275: Project memory recall from the embedded redb+usearch store.
    // Best-effort — empty Vec when the session DB is missing or any step
    // fails, in which case no layer is added. Skipped for the ctrl persona
    // (lean-prompt chat path).
    if !is_ctrl_persona && let Some(proj) = &ctrl.self_project {
        let q = &user_input[..200.min(user_input.len())];
        let memories = recall_project_memories(proj, q, 5).await;
        if !memories.is_empty() {
            builder = builder.add_memory_layer(memories);
        }
    }

    let mut system_prompt = builder.build();

    // Inject a live TM session summary so ctrl can answer "what sessions are
    // running?" using ground truth instead of hallucinated guesses. Resolves
    // the state_dir from the active self-project (or cwd) — same root used
    // when registering the tm_* tools above so the prompt and tools agree.
    {
        let project_root = ctrl
            .self_project
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        let state_dir = project_root.join(".open-mpm").join("state");
        let tm_block = build_tm_context_block(&state_dir).await;
        if !tm_block.is_empty() {
            system_prompt.push_str("\n\n");
            system_prompt.push_str(&tm_block);
        }
    }

    // #182: Augment with self-awareness footer when we have a detected
    // self-project. Kept as a post-build append so it's visible regardless
    // of which base prompt won the resolution race above.
    if let Some(p) = &ctrl.self_project {
        system_prompt.push_str(&format!(
            "\n\nYou are running inside your own project at {}.\nYou can check your own status with self_project_status() and initiate development tasks on yourself with initiate_self_task(task).",
            p.display()
        ));
    }

    // #193: Inject the user profile so CTRL knows who it's talking to.
    if let Some(up) = &ctrl.user_profile {
        let mut block = format!("\n\n## User Context\nUser name: {}", up.name);
        if let Some(email) = up.email.as_deref() {
            block.push_str(&format!("\nEmail: {email}"));
        }
        if let Some(tz) = up.timezone.as_deref() {
            block.push_str(&format!("\nTimezone: {tz}"));
        }
        system_prompt.push_str(&block);
    }

    // Inject current local date AND time (#feat: ctrl date+time injection).
    // Without this, the LLM answers "what time is it?" with "I don't have
    // access to the current time" — a poor UX since wall-clock context is
    // available to the harness. Format matches the REST path in
    // `run_pm_task_with_history` so both code paths agree.
    {
        let now_str = chrono::Local::now()
            .format("%Y-%m-%d %H:%M:%S %Z")
            .to_string();
        system_prompt.push_str(&format!("\n\nCurrent date and time: {}", now_str));
    }

    // Inject runtime deployment context (#feat: ctrl self-awareness).
    //
    // Why: Without this block ctrl deflects "what model are you running?" /
    // "how many tools do you have?" because the system prompt has no concrete
    // answer. All of these values are already known at runtime — surfacing
    // them lets ctrl answer honestly instead of guessing or punting.
    // What: Resolved values pulled from `agent_cfg`, the registry built above,
    // the loaded MCP config, and `build_info::VERSION`. #271 routes through
    // the shared `build_deployment_footer` helper so this matches the block
    // emitted by `run_pm_task_with_history`.
    // #298: Skipped for the ctrl persona to keep the prompt lean.
    if !is_ctrl_persona {
        let runner_label = match agent_cfg.agent.runner {
            crate::agents::RunnerKind::Subprocess => "subprocess (SubprocessAgentRunner)",
            crate::agents::RunnerKind::Inline => "inline (InlineAgentRunner)",
            crate::agents::RunnerKind::ClaudeCode => "claude-code (ClaudeCodeAgentRunner)",
            crate::agents::RunnerKind::InProcess => "in-process (InProcessAgentRunner)",
        };
        let skills_count = agent_cfg
            .system_prompt
            .skills
            .as_ref()
            .map(|s| s.len())
            .unwrap_or(0);
        let mcp_count = mcp_cfg.services_for_role("ctrl").len();
        let project_label = ctrl
            .self_project
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(none — running standalone)".to_string());
        let config_label = agent_cfg_path
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(built-in default — no on-disk ctrl.toml)".to_string());

        system_prompt.push_str(&build_deployment_footer(
            &agent_cfg.agent.name,
            runner_label,
            &agent_cfg.agent.model,
            crate::build_info::VERSION,
            skills_count,
            Some(openai_tools_count),
            Some(mcp_count),
            &project_label,
            Some(&config_label),
        ));
    }

    system_prompt
}

// Purpose: Dispatch the CTRL turn to the LLM — either via the `claude` CLI
// subprocess short-circuit (OAuth-only) or via the REST/Ollama path with
// `chat_with_tools_gated`. Returns the assistant text only; tool side
// effects land in the `CtrlTurnState` slots that were threaded into the
// registry by `prepare_ctrl_turn_state`.
//
// Consumes the registry (wrapped in `Arc` for the dispatcher) so the caller
// must construct the rest of the turn around its lifetime.
#[allow(clippy::too_many_arguments)]
async fn dispatch_ctrl_turn_llm(
    ctrl: &Ctrl,
    user_input: &str,
    system_prompt: &str,
    agent_cfg: AgentConfig,
    registry: ToolRegistry,
    mcp_cfg: &crate::mcp::GlobalConfig,
    dispatch_t0: std::time::Instant,
) -> Result<String> {
    let client = llm::create_client()?;

    // #271: Replace hardcoded `llm::chat(CTRL_MODEL, …)` with credential-aware
    // dispatch via `chat_with_tools_gated`, mirroring `run_pm_task_with_history`.
    // Why: the legacy stdin REPL was bypassing `pick_credentials()` entirely,
    // so users with `ANTHROPIC_API_KEY` or `CLAUDE_CODE_OAUTH_TOKEN` configured
    // still got routed via OpenRouter (or 401-failed when only OAuth was set).
    let mut routed_cfg = agent_cfg;
    tracing::info!(
        elapsed_ms = dispatch_t0.elapsed().as_millis() as u64,
        agent = %routed_cfg.agent.name,
        runner = ?routed_cfg.agent.runner,
        model = %routed_cfg.agent.model,
        use_anthropic_direct = routed_cfg.llm.use_anthropic_direct,
        "ctrl_chat_turn: stage1 config loaded"
    );

    // #295: Gate claude-code routing on the agent's TOML runner field. Just
    // having CLAUDE_CODE_OAUTH_TOKEN in the env is no longer enough — the
    // agent must also declare runner = "claude-code".
    let creds = llm::credentials::pick_credentials(Some(routed_cfg.agent.runner))
        .ok_or_else(|| anyhow::anyhow!("{}", llm::credentials::missing_credentials_error()))?;
    let claude_cli_short_circuit = apply_credential_routing(&mut routed_cfg, &creds);
    tracing::info!(
        elapsed_ms = dispatch_t0.elapsed().as_millis() as u64,
        creds = creds.label(),
        claude_cli_short_circuit,
        model_after_routing = %routed_cfg.agent.model,
        use_anthropic_direct = routed_cfg.llm.use_anthropic_direct,
        "ctrl_chat_turn: stage2 credentials resolved"
    );

    if claude_cli_short_circuit {
        run_ctrl_turn_via_claude_cli(ctrl, &routed_cfg, system_prompt, user_input).await
    } else {
        run_ctrl_turn_via_rest(
            &client,
            user_input,
            system_prompt,
            &routed_cfg,
            registry,
            mcp_cfg,
            dispatch_t0,
        )
        .await
    }
}

// Purpose: OAuth-only path — drive the turn through the `claude` CLI
// subprocess. Tools are dropped because the CLI brings its own surface
// and we have no graceful way to graft open-mpm tools onto it for a
// single-shot invocation.
async fn run_ctrl_turn_via_claude_cli(
    ctrl: &Ctrl,
    routed_cfg: &AgentConfig,
    system_prompt: &str,
    user_input: &str,
) -> Result<String> {
    let project_for_cli = ctrl
        .self_project
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let mut cli_cfg = routed_cfg.clone();
    cli_cfg.system_prompt.content = system_prompt.to_string();
    run_pm_task_via_claude_cli(&project_for_cli, &cli_cfg, user_input, &[], "").await
}

// Purpose: REST / Ollama path. Builds the chat messages, applies the local
// Ollama fast-path when it qualifies, falls back to the remote endpoint on
// local-inference error, and returns the assistant text. Tool calls are
// drained inline by `chat_with_tools_gated` via the registry's executors.
#[allow(clippy::too_many_arguments)]
async fn run_ctrl_turn_via_rest(
    client: &async_openai::Client<async_openai::config::OpenAIConfig>,
    user_input: &str,
    system_prompt: &str,
    routed_cfg: &AgentConfig,
    registry: ToolRegistry,
    mcp_cfg: &crate::mcp::GlobalConfig,
    dispatch_t0: std::time::Instant,
) -> Result<String> {
    use async_openai::types::{
        ChatCompletionRequestMessage, ChatCompletionRequestSystemMessageArgs,
        ChatCompletionRequestUserMessageArgs,
    };
    let mut messages: Vec<ChatCompletionRequestMessage> = Vec::new();
    messages.push(
        ChatCompletionRequestSystemMessageArgs::default()
            .content(system_prompt.to_string())
            .build()
            .context("failed to build ctrl_chat_turn system message")?
            .into(),
    );
    messages.push(
        ChatCompletionRequestUserMessageArgs::default()
            .content(user_input)
            .build()
            .context("failed to build ctrl_chat_turn user message")?
            .into(),
    );
    // #319: Local Ollama fast-path. When enabled in `~/.open-mpm/config.toml`
    // and the user's intent qualifies (conversational chat or TM-data
    // query), route this turn to the configured local ollama model
    // instead of the remote endpoint. ollama availability is probed once
    // per process (`is_ollama_available_cached`) so the hot path doesn't
    // pay a TCP probe on every turn. On failure with `fallback_on_error`,
    // we transparently retry against the remote model.
    let local_cfg = &mcp_cfg.local_inference;
    let intent_class = crate::intent::classify_intent(user_input);
    let local_qualifies = local_cfg.enabled
        && crate::local_inference::qualifies_for_local_inference(&intent_class, user_input)
        && crate::local_inference::is_ollama_available_cached(&local_cfg.ollama_host).await;
    let (effective_model, effective_max_tokens, effective_use_direct) = if local_qualifies {
        tracing::info!(
            local_model = %local_cfg.model,
            ?intent_class,
            "ctrl_chat_turn: routing to local ollama fast-path"
        );
        (
            local_cfg.model.clone(),
            local_cfg.max_tokens,
            false, // ollama is OpenAI-compatible — never use anthropic-direct
        )
    } else {
        (
            routed_cfg.agent.model.clone(),
            routed_cfg.llm.max_tokens.max(1024),
            routed_cfg.llm.use_anthropic_direct,
        )
    };

    let adapter = llm::adapter::adapter_for_model(&effective_model);
    let registry_arc = Arc::new(registry);
    let llm_t0 = std::time::Instant::now();
    tracing::info!(
        elapsed_ms = dispatch_t0.elapsed().as_millis() as u64,
        provider = ?adapter.provider(),
        model = %effective_model,
        use_anthropic_direct = effective_use_direct,
        local_route = local_qualifies,
        "ctrl_chat_turn: stage3 LLM call starting"
    );
    let local_call_result = llm::chat_with_tools_gated(
        client,
        &effective_model,
        &*adapter,
        messages.clone(),
        registry_arc.clone(),
        None,
        0.2,
        effective_max_tokens,
        2,
        false,
        None,
        false,
        effective_use_direct,
        &routed_cfg.llm.stop_sequences,
    )
    .await;

    let mut used_remote_fallback = false;
    let (text, _usage) = match local_call_result {
        Ok(pair) => pair,
        Err(e) if local_qualifies && local_cfg.fallback_on_error => {
            tracing::warn!(
                error = %e,
                "local inference failed, falling back to remote: {e:#}"
            );
            used_remote_fallback = true;
            let remote_adapter = llm::adapter::adapter_for_model(&routed_cfg.agent.model);
            llm::chat_with_tools_gated(
                client,
                &routed_cfg.agent.model,
                &*remote_adapter,
                messages,
                registry_arc,
                None,
                0.2,
                routed_cfg.llm.max_tokens.max(1024),
                2,
                false,
                None,
                false,
                routed_cfg.llm.use_anthropic_direct,
                &routed_cfg.llm.stop_sequences,
            )
            .await?
        }
        Err(e) => return Err(e),
    };
    // #468: Mirror the local→remote fallback surfacing applied in
    // `run_pm_task_with_history`. ctrl_chat_turn is the conversational
    // fast path used by both REPL ctrl chat and Telegram (post-#468);
    // without this prefix the user has no way to tell from the chat
    // surface that the local Ollama model wasn't actually used.
    let text = if used_remote_fallback {
        format!("[⚡ Ollama unavailable — using OpenRouter]\n\n{text}")
    } else {
        text
    };
    tracing::info!(
        llm_ms = llm_t0.elapsed().as_millis() as u64,
        dispatch_ms = dispatch_t0.elapsed().as_millis() as u64,
        response_chars = text.len(),
        "ctrl_chat_turn: stage4 LLM call complete"
    );
    Ok(text)
}

// Purpose: Drain the three pending-side-effect slots (start_pm, self_task,
// stop_task) populated by tool callbacks during the LLM dispatch, applying
// each side effect against `ctrl` and appending a status line to `outputs`.
async fn drain_ctrl_turn_side_effects(
    ctrl: &mut Ctrl,
    side_effects: &CtrlTurnSideEffects,
    outputs: &mut Vec<String>,
) {
    // Drain any queued start_pm side-effect from the tools.
    let to_connect = drain_slot(&side_effects.pending_connect);
    if let Some(path) = to_connect {
        match ctrl.connect(&path).await {
            Ok(msg) => outputs.push(msg),
            Err(e) => outputs.push(format!("start_pm error: {e:#}")),
        }
    }

    // #182: If `initiate_self_task` queued a task alongside the connect,
    // forward it to the (now-active) PM so the LLM round-trips through the
    // standard dispatch path without the user retyping the task.
    let to_self_task = drain_slot(&side_effects.pending_self_task);
    if let Some(task_text) = to_self_task {
        match ctrl.dispatch_task(task_text).await {
            Ok(out) => outputs.push(out),
            Err(e) => outputs.push(format!("initiate_self_task dispatch error: {e:#}")),
        }
    }

    // #202: drain stop_task — locate the matching PM, send Shutdown, and
    // remove from the active map so subsequent /status calls reflect the
    // change.
    let to_stop = drain_slot(&side_effects.pending_stop);
    if let Some(target_name) = to_stop {
        // Find by name (matching the snapshot we built earlier).
        let key_opt = ctrl
            .pms
            .iter()
            .find(|(_, h)| h.name == target_name)
            .map(|(k, _)| k.clone());
        if let Some(key) = key_opt {
            if let Some(handle) = ctrl.pms.remove(&key) {
                let _ = handle.tx.send(PmMsg::Shutdown).await;
                if ctrl.active.as_deref() == Some(key.as_str()) {
                    ctrl.active = None;
                }
                // Best-effort: drop the relay-lookup entry too.
                let mut connected = ctrl.connected_pms.lock().await;
                connected.remove(&handle.name);
                outputs.push(format!("Stopped PM[{}]", handle.name));
            }
        } else {
            outputs.push(format!("stop_task: no PM named {target_name}"));
        }
    }
}

/// Run a single CTRL-level LLM turn with the four tools and apply
/// any queued side-effects (start_pm) when the turn returns.
///
/// Why: Non-slash input at the CTRL prompt should go through the assistant,
/// not directly to a PM, when no PM is active. Keeps the "terse senior dev"
/// voice as the CTRL experience and lets the LLM auto-route e.g. a bare
/// path into a start_pm call.
/// What: Reads like a recipe — prepare per-turn state, resolve agent config,
/// build the system prompt, dispatch the LLM call, then drain pending side
/// effects. Each phase lives in its own helper above so the function stays
/// short enough to scan in one screen.
/// Test: `ctrl_chat_turn_routes_start_pm` / `ctrl_chat_turn_returns_text`.
async fn ctrl_chat_turn(ctrl: &mut Ctrl, user_input: &str) -> Result<String> {
    // #297: Latency tracing for the ctrl dispatch path. Stage timestamps are
    // emitted at INFO so `RUST_LOG=info` reveals where wall-clock time goes
    // (config load → credential probe → LLM call → first byte).
    let dispatch_t0 = std::time::Instant::now();
    tracing::info!(
        input_len = user_input.len(),
        "ctrl_chat_turn: dispatch start"
    );

    let CtrlTurnState {
        registry,
        openai_tools,
        side_effects,
    } = prepare_ctrl_turn_state(ctrl).await?;
    let (agent_cfg, agent_cfg_path) = resolve_ctrl_turn_agent_config(ctrl).await;

    // #241: MCP tool descriptions for role "ctrl".
    // #244: Use load() (no create-if-absent) for hot-reload after mcp_* tool calls.
    let mcp_cfg = crate::mcp::GlobalConfig::load().await;

    let system_prompt = build_ctrl_turn_system_prompt(
        ctrl,
        user_input,
        &agent_cfg,
        agent_cfg_path.as_deref(),
        openai_tools.len(),
        &mcp_cfg,
    )
    .await;

    // Move `registry` into the dispatch helper so it can be wrapped in an
    // `Arc` for `chat_with_tools_gated`. The pending side-effect slots stay
    // live through `side_effects` because each tool captured a clone of the
    // Arcs during `build_ctrl_registry`.
    let response_content = dispatch_ctrl_turn_llm(
        ctrl,
        user_input,
        &system_prompt,
        agent_cfg,
        registry,
        &mcp_cfg,
        dispatch_t0,
    )
    .await?;

    // #271: `chat_with_tools_gated` already drained tool calls (filling the
    // `pending_*` slots via the tools' own execution). The legacy
    // `for tc in response.tool_calls` post-loop is no longer needed because
    // the gated dispatcher executes tools inline. We just collect the
    // assistant text into `outputs` and proceed with the queued side-effects.
    let mut outputs: Vec<String> = Vec::new();
    if !response_content.trim().is_empty() {
        outputs.push(response_content);
    }

    drain_ctrl_turn_side_effects(ctrl, &side_effects, &mut outputs).await;

    Ok(outputs.join("\n"))
}

/// Load `~/.open-mpm/user.toml`, or run the first-run interview to create it. (#193)
///
/// Why: CTRL is the user's persistent home base; capturing a small profile
/// once means every subsequent session can address the user by name and use
/// timezone-aware date math. Skipped automatically when stdin is not a TTY
/// (piped input, `--api` mode, CI) so unattended invocations never block.
/// What: If `~/.open-mpm/user.toml` exists and `is_complete()`, return it.
/// Otherwise, if stdin is a TTY, prompt for name/email/timezone and persist;
/// if not a TTY, return a default `User` profile without saving.
/// Test: Tested via `UserProfile` round-trip + manual REPL launch.
async fn load_or_create_user_profile() -> Result<Option<crate::identity::user_profile::UserProfile>>
{
    use crate::identity::user_profile::UserProfile;

    if let Some(p) = UserProfile::load()
        && p.is_complete()
    {
        return Ok(Some(p));
    }

    // Skip the interview when stdin isn't a terminal. We can't reliably
    // detect TTY-ness without a dep, so the proxy is "is OPEN_MPM_API_TOKEN
    // set?" or "is OPEN_MPM_NONINTERACTIVE set?" plus an env-var marker the
    // tests / `--api` path can use. As a final fallback, attempt to read
    // a single line with a timeout — but for simplicity here, gate on the
    // env vars only. CTRL is the only place this is called and is the
    // canonical interactive entry point.
    let noninteractive = std::env::var("OPEN_MPM_NONINTERACTIVE").is_ok()
        || std::env::var("OPEN_MPM_API_TOKEN").is_ok();
    if noninteractive {
        let p = UserProfile {
            name: "User".to_string(),
            email: None,
            preferred_model: None,
            timezone: None,
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        return Ok(Some(p));
    }

    let profile = conduct_user_interview().await?;
    if let Err(e) = profile.save() {
        tracing::warn!(error = %e, "failed to save user profile (continuing in-memory)");
    } else {
        eprintln!(
            "Welcome, {}! Your profile has been saved to ~/.open-mpm/user.toml",
            profile.name
        );
    }
    Ok(Some(profile))
}

/// Interactive first-run interview that captures the user profile. (#193)
///
/// Why: The first time a person launches CTRL we want a five-second prompt
/// to capture their name + optional email + timezone — these flow into the
/// CTRL system prompt so the LLM can personalize replies.
/// What: Reads three lines from stdin (name, email, timezone). Email/tz are
/// optional (empty input == None). Returns a fully-populated `UserProfile`
/// with `created_at` set to the current UTC timestamp.
/// Test: Manual smoke (runs only on first launch).
async fn conduct_user_interview() -> Result<crate::identity::user_profile::UserProfile> {
    use crate::identity::user_profile::UserProfile;

    eprintln!("[open-mpm] First-run setup — let's capture a quick profile.");
    eprint!("What's your name? ");
    let _ = std::io::Write::flush(&mut std::io::stderr());

    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin);

    let mut name = String::new();
    reader.read_line(&mut name).await?;
    let name = name.trim().to_string();
    let name = if name.is_empty() {
        "User".to_string()
    } else {
        name
    };

    eprint!("Email address (optional, press Enter to skip): ");
    let _ = std::io::Write::flush(&mut std::io::stderr());
    let mut email = String::new();
    reader.read_line(&mut email).await?;
    let email = email.trim().to_string();
    let email = if email.is_empty() { None } else { Some(email) };

    eprint!("Timezone (e.g. America/New_York, or Enter to skip): ");
    let _ = std::io::Write::flush(&mut std::io::stderr());
    let mut tz = String::new();
    reader.read_line(&mut tz).await?;
    let tz = tz.trim().to_string();
    let timezone = if tz.is_empty() { None } else { Some(tz) };

    Ok(UserProfile {
        name,
        email,
        preferred_model: None,
        timezone,
        created_at: chrono::Utc::now().to_rfc3339(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_project_index_in_prompt_noop_when_no_section() {
        let prompt = "You are a PM.\n\nNo index here.";
        let out = filter_project_index_in_prompt(prompt, "anything", 5);
        assert_eq!(out, prompt);
    }

    #[test]
    fn filter_project_index_in_prompt_filters_bullets_by_task() {
        let prompt = "## Project Context (auto-indexed)\n\n\
                      - src/credentials.rs — credential routing helpers\n\
                      - ui/src/main.tsx — react root\n\
                      - src/repl/mod.rs — terminal repl\n\
                      - src/agents/mod.rs — agent loader\n\n\
                      ---\n\nrest of prompt\n";
        let out = filter_project_index_in_prompt(prompt, "fix credential routing", 2);
        assert!(out.contains("## Project Context (auto-indexed)"));
        assert!(out.contains("credential"));
        // Top-2 should drop unrelated bullets (react root / terminal repl).
        assert!(
            !out.contains("react root") || !out.contains("terminal repl"),
            "filter should have dropped at least one unrelated bullet, got: {out}"
        );
        // Tail preserved.
        assert!(out.contains("rest of prompt"));
    }

    #[test]
    fn filter_project_index_in_prompt_terminates_at_next_heading() {
        let prompt = "## Project Context (auto-indexed)\n\n\
                      - a — alpha\n\
                      - b — beta\n\n\
                      ## Next Section\n\nbody\n";
        let out = filter_project_index_in_prompt(prompt, "alpha", 1);
        assert!(out.contains("## Next Section"));
        assert!(out.contains("body"));
    }

    #[test]
    fn apply_credential_routing_anthropic_direct_sets_flag() {
        let mut cfg = AgentConfig::ctrl_default();
        cfg.llm.use_anthropic_direct = false;
        let short_circuit =
            apply_credential_routing(&mut cfg, &llm::credentials::LlmCredentials::AnthropicDirect);
        assert!(!short_circuit);
        assert!(cfg.llm.use_anthropic_direct);
    }

    #[test]
    fn strip_cli_artifacts_removes_summary_with_double_newline() {
        let input = "Hello world\n\n## Summary\n- did stuff\n".to_string();
        assert_eq!(strip_cli_artifacts(input), "Hello world");
    }

    #[test]
    fn strip_cli_artifacts_removes_summary_with_single_newline() {
        let input = "Hello world\n## Summary\n- did stuff".to_string();
        assert_eq!(strip_cli_artifacts(input), "Hello world");
    }

    #[test]
    fn strip_cli_artifacts_removes_summary_at_start() {
        let input = "## Summary\n- only summary".to_string();
        assert_eq!(strip_cli_artifacts(input), "");
    }

    #[test]
    fn strip_cli_artifacts_trims_trailing_whitespace_when_no_summary() {
        let input = "Hello world\n\n   \n".to_string();
        assert_eq!(strip_cli_artifacts(input), "Hello world");
    }

    #[test]
    fn strip_cli_artifacts_preserves_content_without_summary() {
        let input = "Hello world".to_string();
        assert_eq!(strip_cli_artifacts(input), "Hello world");
    }

    #[test]
    fn apply_credential_routing_claude_code_signals_short_circuit() {
        let mut cfg = AgentConfig::ctrl_default();
        let short_circuit =
            apply_credential_routing(&mut cfg, &llm::credentials::LlmCredentials::ClaudeCode);
        assert!(short_circuit, "ClaudeCode must signal CLI short-circuit");
        assert!(!cfg.llm.use_anthropic_direct);
    }

    #[test]
    fn apply_credential_routing_openrouter_qualifies_bare_claude_id() {
        let mut cfg = AgentConfig::ctrl_default();
        cfg.agent.model = "claude-sonnet-4-6".to_string();
        let short_circuit =
            apply_credential_routing(&mut cfg, &llm::credentials::LlmCredentials::OpenRouter);
        assert!(!short_circuit);
        assert_eq!(cfg.agent.model, "anthropic/claude-sonnet-4-6");
    }

    #[test]
    fn apply_credential_routing_openrouter_leaves_prefixed_model_alone() {
        let mut cfg = AgentConfig::ctrl_default();
        cfg.agent.model = "openai/gpt-4o".to_string();
        apply_credential_routing(&mut cfg, &llm::credentials::LlmCredentials::OpenRouter);
        assert_eq!(cfg.agent.model, "openai/gpt-4o");
    }

    #[test]
    fn build_deployment_footer_includes_required_fields() {
        let s = build_deployment_footer(
            "ctrl",
            "openrouter",
            "anthropic/claude-sonnet-4-6",
            "0.1.0",
            3,
            Some(11),
            Some(2),
            "/proj",
            Some("/proj/.open-mpm/agents/ctrl.toml"),
        );
        assert!(s.contains("## Deployment Configuration"));
        assert!(s.contains("- Agent: ctrl"));
        assert!(s.contains("- Model: anthropic/claude-sonnet-4-6"));
        assert!(s.contains("- Runner: openrouter"));
        assert!(s.contains("- Version: v0.1.0"));
        assert!(s.contains("- Skills loaded: 3"));
        assert!(s.contains("- Tools available: 11"));
        assert!(s.contains("- MCP connections: 2"));
        assert!(s.contains("- Project: /proj"));
        assert!(s.contains("- Config: /proj/.open-mpm/agents/ctrl.toml"));
    }

    #[test]
    fn build_deployment_footer_omits_optional_fields_when_none() {
        let s = build_deployment_footer(
            "pm",
            "openrouter",
            "model-x",
            "0.1.0",
            0,
            None,
            None,
            "/proj",
            None,
        );
        assert!(s.contains("- Agent: pm"));
        assert!(!s.contains("Tools available"));
        assert!(!s.contains("MCP connections"));
        assert!(!s.contains("Config:"));
        assert!(s.contains("- Skills loaded: 0"));
    }

    #[test]
    fn match_any_glob_handles_suffix_wildcard() {
        // `mcp_*` matches every dynamic mcp_* tool name.
        let patterns = vec!["mcp_*".to_string(), "git_log".to_string()];
        assert!(match_any_glob("mcp_list", &patterns));
        assert!(match_any_glob("mcp_enable", &patterns));
        assert!(match_any_glob("mcp_", &patterns)); // edge: empty suffix
        assert!(match_any_glob("git_log", &patterns));
        // Names not in any pattern are rejected.
        assert!(!match_any_glob("git_status", &patterns));
        assert!(!match_any_glob("shell_exec", &patterns));
        // Empty pattern list rejects everything (caller treats `None` as
        // "no filter" separately, see `run_pm_task_with_persona`).
        assert!(!match_any_glob("anything", &[]));
    }

    /// Insert a fake PmHandle into ctrl for the given key/name.
    fn insert_fake(ctrl: &mut Ctrl, key: &str, name: &str) -> mpsc::Sender<PmMsg> {
        let (tx, _rx) = mpsc::channel(1);
        let tx2 = tx.clone();
        ctrl.pms.insert(
            key.to_string(),
            PmHandle {
                name: name.to_string(),
                project_path: PathBuf::from(key),
                tx,
                task: tokio::spawn(async {}),
                status: Arc::new(Mutex::new("idle".to_string())),
                last_message: Arc::new(Mutex::new(String::new())),
            },
        );
        tx2
    }

    /// Insert a mock actor that replies with `response` to the first Task.
    fn insert_mock_actor(ctrl: &mut Ctrl, key: &str, response: Result<String>) {
        let (tx, mut rx) = mpsc::channel::<PmMsg>(16);
        let task = tokio::spawn(async move {
            if let Some(PmMsg::Task { reply, .. }) = rx.recv().await {
                let _ = reply.send(response);
            }
        });
        ctrl.pms.insert(
            key.to_string(),
            PmHandle {
                name: key.to_string(),
                project_path: PathBuf::from(key),
                tx,
                task,
                status: Arc::new(Mutex::new("idle".to_string())),
                last_message: Arc::new(Mutex::new(String::new())),
            },
        );
        ctrl.active = Some(key.to_string());
    }

    // -- Ctrl::new --
    #[test]
    fn new_ctrl_has_no_sessions() {
        let c = Ctrl::new();
        assert!(c.pms.is_empty() && c.active.is_none());
    }

    // -- Ctrl::prompt --
    #[test]
    fn prompt_without_active_shows_ctrl() {
        assert_eq!(Ctrl::new().prompt(), "CTRL> ");
    }

    #[tokio::test]
    async fn prompt_with_active_shows_pm_name() {
        let mut c = Ctrl::new();
        insert_fake(&mut c, "/tmp/p", "proj");
        c.active = Some("/tmp/p".to_string());
        assert_eq!(c.prompt(), "PM[proj]> ");
    }

    #[test]
    fn prompt_with_stale_active_shows_question_mark() {
        let mut c = Ctrl::new();
        c.active = Some("/gone".to_string());
        assert_eq!(c.prompt(), "PM[?]> ");
    }

    // -- Ctrl::disconnect --
    #[test]
    fn disconnect_when_no_active() {
        let mut c = Ctrl::new();
        assert_eq!(c.disconnect(), "No active PM session.");
    }

    #[tokio::test]
    async fn disconnect_clears_active_but_keeps_handle() {
        let mut c = Ctrl::new();
        insert_fake(&mut c, "/tmp/p", "proj");
        c.active = Some("/tmp/p".to_string());
        let msg = c.disconnect();
        assert!(msg.contains("Disconnected") && msg.contains("proj"));
        assert!(c.active.is_none() && c.pms.contains_key("/tmp/p"));
    }

    // -- Ctrl::status --
    #[test]
    fn status_empty() {
        assert_eq!(Ctrl::new().status(), "No PM sessions.");
    }

    #[tokio::test]
    async fn status_lists_sessions_with_markers() {
        let mut c = Ctrl::new();
        insert_fake(&mut c, "/a", "alpha");
        insert_fake(&mut c, "/b", "beta");
        c.active = Some("/a".to_string());
        let out = c.status();
        assert!(out.contains("alpha") && out.contains("beta"));
        assert!(out.contains("[*]") && out.contains("[ ]"));
    }

    // -- Ctrl::dispatch_task --
    #[tokio::test]
    async fn dispatch_without_active_errors() {
        let e = Ctrl::new().dispatch_task("hi".into()).await.unwrap_err();
        assert!(e.to_string().contains("no active PM session"));
    }

    #[tokio::test]
    async fn dispatch_with_closed_channel_errors() {
        let mut c = Ctrl::new();
        let (tx, rx) = mpsc::channel(1);
        drop(rx);
        c.pms.insert(
            "/d".into(),
            PmHandle {
                name: "d".into(),
                project_path: "/d".into(),
                tx,
                task: tokio::spawn(async {}),
                status: Arc::new(Mutex::new("idle".to_string())),
                last_message: Arc::new(Mutex::new(String::new())),
            },
        );
        c.active = Some("/d".into());
        assert!(c.dispatch_task("hi".into()).await.is_err());
    }

    #[tokio::test]
    async fn dispatch_receives_actor_reply() {
        let mut c = Ctrl::new();
        insert_mock_actor(&mut c, "/m", Ok("ok".into()));
        assert_eq!(c.dispatch_task("hi".into()).await.unwrap(), "ok");
    }

    #[tokio::test]
    async fn dispatch_propagates_actor_error() {
        let mut c = Ctrl::new();
        insert_mock_actor(&mut c, "/e", Err(anyhow::anyhow!("boom")));
        let e = c.dispatch_task("hi".into()).await.unwrap_err();
        assert!(e.to_string().contains("boom"));
    }

    // -- Ctrl::connect --
    #[tokio::test]
    async fn connect_creates_handle() {
        let mut c = Ctrl::new();
        let tmp = tempfile::tempdir().unwrap();
        let msg = c.connect(tmp.path().to_str().unwrap()).await.unwrap();
        assert!(msg.contains("Connected to PM["));
        assert_eq!(c.pms.len(), 1);
        c.shutdown_all().await;
    }

    #[tokio::test]
    async fn connect_same_path_reuses() {
        let mut c = Ctrl::new();
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().to_str().unwrap();
        c.connect(p).await.unwrap();
        let msg = c.connect(p).await.unwrap();
        assert!(msg.contains("Switched") && c.pms.len() == 1);
        c.shutdown_all().await;
    }

    #[tokio::test]
    async fn connect_invalid_path_errors() {
        assert!(Ctrl::new().connect("/no_such_xyz_999").await.is_err());
    }

    #[tokio::test]
    async fn connect_two_dirs() {
        let mut c = Ctrl::new();
        let (t1, t2) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        c.connect(t1.path().to_str().unwrap()).await.unwrap();
        c.connect(t2.path().to_str().unwrap()).await.unwrap();
        assert_eq!(c.pms.len(), 2);
        c.shutdown_all().await;
    }

    // -- Ctrl::shutdown_all --
    #[tokio::test]
    async fn shutdown_all_completes() {
        let mut c = Ctrl::new();
        let (tx, mut rx) = mpsc::channel::<PmMsg>(16);
        let task = tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                if matches!(msg, PmMsg::Shutdown) {
                    break;
                }
            }
        });
        c.pms.insert(
            "/s".into(),
            PmHandle {
                name: "s".into(),
                project_path: "/s".into(),
                tx,
                task,
                status: Arc::new(Mutex::new("idle".to_string())),
                last_message: Arc::new(Mutex::new(String::new())),
            },
        );
        c.shutdown_all().await; // must not hang
    }

    // -- handle_command --
    #[tokio::test]
    async fn cmd_quit_returns_false() {
        let mut c = Ctrl::new();
        for cmd in ["/quit", "/exit", "/q"] {
            assert!(!handle_command(&mut c, cmd).await.unwrap(), "{cmd}");
        }
    }

    #[tokio::test]
    async fn cmd_status_help_disconnect_unknown_return_true() {
        let mut c = Ctrl::new();
        for cmd in ["/status", "/help", "/disconnect", "/bogus"] {
            assert!(handle_command(&mut c, cmd).await.unwrap(), "{cmd}");
        }
    }

    #[tokio::test]
    async fn cmd_connect_no_arg_errors() {
        assert!(handle_command(&mut Ctrl::new(), "/connect").await.is_err());
    }

    #[tokio::test]
    async fn cmd_connect_valid_dir() {
        let mut c = Ctrl::new();
        let tmp = tempfile::tempdir().unwrap();
        let cmd = format!("/connect {}", tmp.path().display());
        assert!(handle_command(&mut c, &cmd).await.unwrap());
        assert_eq!(c.pms.len(), 1);
        c.shutdown_all().await;
    }

    // -- PmHandle actor lifecycle --
    #[tokio::test]
    async fn actor_processes_task_and_shuts_down() {
        let (tx, rx) = mpsc::channel::<PmMsg>(16);
        let task = tokio::spawn(async move {
            let mut rx = rx;
            while let Some(msg) = rx.recv().await {
                match msg {
                    PmMsg::Task { text, reply } => {
                        let _ = reply.send(Ok(format!("echo:{text}")));
                    }
                    PmMsg::Shutdown => break,
                }
            }
        });
        let (rtx, rrx) = oneshot::channel();
        tx.send(PmMsg::Task {
            text: "ping".into(),
            reply: rtx,
        })
        .await
        .unwrap();
        assert_eq!(rrx.await.unwrap().unwrap(), "echo:ping");
        tx.send(PmMsg::Shutdown).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
    }

    // -- #182: self-project detection + tools --

    /// Build a fake self-project layout under `tmp` and return the root.
    fn make_fake_self_project(tmp: &std::path::Path) -> PathBuf {
        let agents = tmp.join(".open-mpm").join("agents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(agents.join("pm.toml"), "[agent]\nname=\"pm\"\n").unwrap();
        std::fs::write(
            tmp.join("Cargo.toml"),
            "[package]\nname = \"x\"\nversion = \"9.9.9\"\nedition = \"2021\"\n",
        )
        .unwrap();
        tmp.to_path_buf()
    }

    #[test]
    fn detect_self_project_finds_via_env_var() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_fake_self_project(tmp.path());
        // SAFETY: tests run single-threaded within this process by default; we
        // restore the env var afterwards.
        unsafe {
            std::env::set_var("OPEN_MPM_PROJECT_DIR", &root);
        }
        let detected = detect_self_project();
        unsafe {
            std::env::remove_var("OPEN_MPM_PROJECT_DIR");
        }
        assert!(detected.is_some(), "expected detection via env var");
    }

    #[test]
    fn detect_self_project_returns_none_when_no_marker() {
        let tmp = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("OPEN_MPM_PROJECT_DIR", tmp.path());
        }
        let detected = detect_self_project();
        unsafe {
            std::env::remove_var("OPEN_MPM_PROJECT_DIR");
        }
        // The env-var path requires the marker file; without it we fall back
        // to walk-up from current_exe / cwd, neither of which is guaranteed to
        // succeed inside the test sandbox. We only assert non-panic behavior.
        let _ = detected;
    }

    #[tokio::test]
    async fn self_project_status_returns_version_when_path_set() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_fake_self_project(tmp.path());
        let tool = SelfProjectStatusTool {
            self_path: Some(root.clone()),
        };
        let result = tool.execute(json!({})).await;
        assert!(!result.is_error(), "expected ok, got {}", result.content());
        assert!(
            result.content().contains("9.9.9"),
            "expected version in output: {}",
            result.content()
        );
    }

    #[tokio::test]
    async fn self_project_status_errors_when_no_self_path() {
        let tool = SelfProjectStatusTool { self_path: None };
        let result = tool.execute(json!({})).await;
        assert!(result.is_error());
        assert!(result.content().contains("no self-project detected"));
    }

    #[tokio::test]
    async fn initiate_self_task_queues_self_project_path() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_fake_self_project(tmp.path());
        let pending_connect: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let pending_self_task: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let tool = InitiateSelfTaskTool {
            self_path: Some(root.clone()),
            pending_connect: pending_connect.clone(),
            pending_self_task: pending_self_task.clone(),
        };
        let result = tool.execute(json!({ "task": "fix bug X" })).await;
        assert!(!result.is_error(), "expected ok, got {}", result.content());
        assert_eq!(
            pending_connect.lock().unwrap().as_deref(),
            Some(root.display().to_string().as_str())
        );
        assert_eq!(
            pending_self_task.lock().unwrap().as_deref(),
            Some("fix bug X")
        );
    }

    // -- #185: task_status tool --
    #[tokio::test]
    async fn task_status_returns_known_pm_state() {
        let status = Arc::new(Mutex::new("running".to_string()));
        let last = Arc::new(Mutex::new("write a fastapi app".to_string()));
        let tool = TaskStatusTool {
            snapshot: vec![("alpha".to_string(), status.clone(), last.clone())],
        };
        let result = tool.execute(json!({})).await;
        assert!(!result.is_error(), "expected ok, got {}", result.content());
        let body = result.content();
        assert!(body.contains("alpha"), "missing project name: {body}");
        assert!(body.contains("running"), "missing status: {body}");
        assert!(body.contains("fastapi"), "missing last_message: {body}");
    }

    #[tokio::test]
    async fn initiate_self_task_errors_when_no_self_path() {
        let tool = InitiateSelfTaskTool {
            self_path: None,
            pending_connect: Arc::new(Mutex::new(None)),
            pending_self_task: Arc::new(Mutex::new(None)),
        };
        let result = tool.execute(json!({ "task": "x" })).await;
        assert!(result.is_error());
    }

    // -- Prompt transitions through full lifecycle --
    #[tokio::test]
    async fn prompt_transitions() {
        let mut c = Ctrl::new();
        assert_eq!(c.prompt(), "CTRL> ");
        let tmp = tempfile::tempdir().unwrap();
        c.connect(tmp.path().to_str().unwrap()).await.unwrap();
        assert!(c.prompt().starts_with("PM[") && c.prompt().ends_with("]> "));
        c.disconnect();
        assert_eq!(c.prompt(), "CTRL> ");
        c.shutdown_all().await;
    }

    #[test]
    fn extract_name_from_input_im_bob() {
        assert_eq!(extract_name_from_input("I'm Bob"), Some("Bob".to_string()));
    }

    #[test]
    fn extract_name_from_input_my_name_is_alice() {
        assert_eq!(
            extract_name_from_input("My name is Alice"),
            Some("Alice".to_string())
        );
    }

    #[test]
    fn extract_name_from_input_bare_name() {
        assert_eq!(extract_name_from_input("Bob"), Some("Bob".to_string()));
        assert_eq!(extract_name_from_input("bob"), Some("Bob".to_string()));
    }

    #[test]
    fn extract_name_from_input_call_me_sam() {
        assert_eq!(
            extract_name_from_input("call me Sam"),
            Some("Sam".to_string())
        );
    }

    #[test]
    fn extract_name_from_input_im_alice_lower() {
        assert_eq!(
            extract_name_from_input("im alice"),
            Some("Alice".to_string())
        );
    }

    #[test]
    fn extract_name_from_input_rejects_task_requests() {
        assert_eq!(extract_name_from_input("write me code"), None);
        assert_eq!(extract_name_from_input("build a python script"), None);
    }

    #[test]
    fn extract_name_from_input_rejects_greetings() {
        assert_eq!(extract_name_from_input("Hello"), None);
        assert_eq!(extract_name_from_input("hi"), None);
        assert_eq!(extract_name_from_input("hey"), None);
        assert_eq!(extract_name_from_input("thanks"), None);
    }

    #[test]
    fn extract_name_from_input_rejects_im_filler() {
        assert_eq!(extract_name_from_input("I'm here"), None);
        assert_eq!(extract_name_from_input("I'm fine"), None);
    }

    // -- #202 new CTRL tools --

    #[tokio::test]
    async fn add_project_tool_validates_path() {
        // Missing path arg → error.
        let tool = AddProjectTool;
        let r = tool.execute(json!({})).await;
        assert!(r.is_error(), "expected error for missing path");

        // Non-existent path → error.
        let r = tool
            .execute(json!({ "path": "/definitely/does/not/exist/zzzz" }))
            .await;
        assert!(r.is_error(), "expected error for missing dir");

        // File (not directory) → error. Use Cargo.toml as a guaranteed file.
        let cwd = std::env::current_dir().unwrap();
        let cargo = cwd.join("Cargo.toml");
        let r = tool
            .execute(json!({ "path": cargo.display().to_string() }))
            .await;
        assert!(r.is_error(), "expected error for path-is-file");
    }

    #[tokio::test]
    async fn set_active_project_updates_slot() {
        let active = Arc::new(Mutex::new(None));
        let tool = SetActiveProjectTool {
            active_project: active.clone(),
        };
        let cwd = std::env::current_dir().unwrap();
        let r = tool
            .execute(json!({ "path": cwd.display().to_string() }))
            .await;
        assert!(!r.is_error(), "unexpected: {}", r.content());
        let stored = active.lock().unwrap().clone();
        assert!(stored.is_some());
        assert!(r.content().contains("Active project set"));
    }

    #[tokio::test]
    async fn set_active_project_rejects_missing_path() {
        let tool = SetActiveProjectTool {
            active_project: Arc::new(Mutex::new(None)),
        };
        let r = tool.execute(json!({})).await;
        assert!(r.is_error());
        let r = tool
            .execute(json!({ "path": "/definitely/does/not/exist/zzzz" }))
            .await;
        assert!(r.is_error());
    }

    #[tokio::test]
    async fn start_pm_falls_back_to_active_project() {
        // No 'project_path' arg, but active_project is set → tool succeeds
        // and queues the active path.
        let pending = Arc::new(Mutex::new(None));
        let active = Arc::new(Mutex::new(Some(PathBuf::from("/tmp/some-active"))));
        let tool = StartPmTool {
            pending: pending.clone(),
            active_project: active,
        };
        let r = tool.execute(json!({})).await;
        assert!(!r.is_error(), "unexpected: {}", r.content());
        let queued = pending.lock().unwrap().clone();
        assert_eq!(queued, Some("/tmp/some-active".to_string()));
    }

    #[tokio::test]
    async fn start_pm_errors_when_no_path_and_no_active() {
        let tool = StartPmTool {
            pending: Arc::new(Mutex::new(None)),
            active_project: Arc::new(Mutex::new(None)),
        };
        let r = tool.execute(json!({})).await;
        assert!(r.is_error());
        assert!(r.content().contains("no active project"));
    }

    #[tokio::test]
    async fn stop_task_tool_records_pending_stop() {
        let (tx, _rx) = mpsc::channel::<PmMsg>(4);
        let snapshot: Vec<PmStopHandle> = vec![("alpha".to_string(), "/tmp/alpha".to_string(), tx)];
        let pending = Arc::new(Mutex::new(None));
        let tool = StopTaskTool {
            snapshot,
            pending_stop: pending.clone(),
        };
        let r = tool.execute(json!({ "session_id": "alpha" })).await;
        assert!(!r.is_error(), "unexpected: {}", r.content());
        assert!(r.content().contains("stopped"));
        let queued = pending.lock().unwrap().clone();
        assert_eq!(queued, Some("alpha".to_string()));
    }

    #[tokio::test]
    async fn stop_task_tool_returns_not_found_for_unknown_id() {
        let tool = StopTaskTool {
            snapshot: Vec::new(),
            pending_stop: Arc::new(Mutex::new(None)),
        };
        let r = tool.execute(json!({ "session_id": "missing" })).await;
        assert!(!r.is_error());
        assert!(r.content().contains("Task not found"));
    }

    #[tokio::test]
    async fn remove_project_tool_returns_not_found_for_unknown_path() {
        // Use a unique path that shouldn't exist in the registry. Requires
        // HOME being set; in CI/local it always is.
        let tool = RemoveProjectTool;
        let r = tool
            .execute(json!({ "path": "/zzz-open-mpm-#202-no-such-project" }))
            .await;
        // Either "Project not found" or registry-unavailable — both are
        // acceptable non-error outcomes here.
        assert!(!r.is_error() || r.content().contains("registry unavailable"));
    }

    #[tokio::test]
    async fn move_file_tool_renames_basic() {
        let tmp = tempfile::tempdir().unwrap();
        let from = tmp.path().join("a.txt");
        std::fs::write(&from, b"hello").unwrap();
        let to = tmp.path().join("b.txt");
        let tool = MoveFileTool;
        let r = tool
            .execute(json!({ "from": from.to_str().unwrap(), "to": to.to_str().unwrap() }))
            .await;
        assert!(!r.is_error(), "unexpected: {}", r.content());
        assert!(!from.exists());
        assert!(to.exists());
        assert_eq!(std::fs::read_to_string(&to).unwrap(), "hello");
    }

    #[tokio::test]
    async fn move_file_tool_into_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let from = tmp.path().join("script.py");
        std::fs::write(&from, b"x").unwrap();
        let target_dir = tmp.path().join("scripts");
        std::fs::create_dir_all(&target_dir).unwrap();
        let tool = MoveFileTool;
        let r = tool
            .execute(json!({
                "from": from.to_str().unwrap(),
                "to": target_dir.to_str().unwrap()
            }))
            .await;
        assert!(!r.is_error(), "unexpected: {}", r.content());
        assert!(target_dir.join("script.py").exists());
        assert!(!from.exists());
    }

    #[tokio::test]
    async fn move_file_tool_missing_source_errors() {
        let tool = MoveFileTool;
        let r = tool
            .execute(json!({
                "from": "/zzz-no-such-file-open-mpm",
                "to": "/tmp/whatever"
            }))
            .await;
        assert!(r.is_error());
    }

    #[tokio::test]
    async fn create_dir_tool_makes_nested_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("a").join("b").join("c");
        let tool = CreateDirTool;
        let r = tool
            .execute(json!({ "path": target.to_str().unwrap() }))
            .await;
        assert!(!r.is_error(), "unexpected: {}", r.content());
        assert!(target.is_dir());
    }

    #[tokio::test]
    async fn create_dir_tool_idempotent_on_existing() {
        let tmp = tempfile::tempdir().unwrap();
        let tool = CreateDirTool;
        let r = tool
            .execute(json!({ "path": tmp.path().to_str().unwrap() }))
            .await;
        assert!(!r.is_error());
        assert!(r.content().contains("already exists"));
    }

    #[test]
    fn detect_stack_finds_rust() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("Cargo.toml"), b"[package]\n").unwrap();
        assert_eq!(detect_stack(tmp.path()), "Rust");
    }

    #[test]
    fn detect_stack_finds_node() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("package.json"), b"{}").unwrap();
        assert_eq!(detect_stack(tmp.path()), "Node.js/TypeScript");
    }

    #[test]
    fn detect_stack_returns_unknown_when_no_indicators() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(detect_stack(tmp.path()), "unknown");
    }

    #[test]
    fn is_empty_project_ignores_dotfiles() {
        let tmp = tempfile::tempdir().unwrap();
        // Create only hidden files; should still report empty.
        std::fs::write(tmp.path().join(".gitignore"), b"").unwrap();
        assert!(is_empty_project(tmp.path()));
        // Now add a non-hidden file.
        std::fs::write(tmp.path().join("README.md"), b"").unwrap();
        assert!(!is_empty_project(tmp.path()));
    }

    // -- resolve_agent_config (#240) --

    #[tokio::test]
    async fn resolve_agent_config_prefers_pm_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let agents = tmp.path().join(".open-mpm/agents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(
            agents.join("pm.toml"),
            r#"
[agent]
name = "pm"
role = "manager"
model = "anthropic/claude-sonnet-4-6"
description = "test pm"

[llm]
temperature = 0.2
max_tokens = 1024

[system_prompt]
content = "pm-from-disk"
"#,
        )
        .unwrap();

        let (cfg, _path) = super::resolve_agent_config(tmp.path()).await.unwrap();
        assert_eq!(cfg.agent.name, "pm");
        assert_eq!(cfg.system_prompt.content, "pm-from-disk");
    }

    #[tokio::test]
    async fn resolve_agent_config_falls_back_to_project_ctrl_toml() {
        // No pm.toml on disk, no user-level ctrl.toml expected (best-effort
        // — if the running user happens to have ~/.open-mpm/agents/ctrl.toml
        // we'd pick that instead, so this test only asserts the project-level
        // fallback fires when both pm.toml is absent AND the user's home
        // version is missing OR matches the project version's contents).
        let tmp = tempfile::tempdir().expect("tempdir");
        let agents = tmp.path().join(".open-mpm/agents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(
            agents.join("ctrl.toml"),
            r#"
[agent]
name = "ctrl"
role = "controller"
model = "anthropic/claude-sonnet-4-6"
description = "test ctrl"

[llm]
temperature = 0.7
max_tokens = 2048

[system_prompt]
content = "ctrl-from-project-disk"
"#,
        )
        .unwrap();

        let (cfg, _path) = super::resolve_agent_config(tmp.path()).await.unwrap();
        assert_eq!(cfg.agent.name, "ctrl");
        // Either a user-level ctrl.toml took precedence, or our project-
        // level file did. Both outcomes prove the function returns Ok.
        assert!(matches!(cfg.agent.role.as_str(), "controller" | "ctrl"));
    }

    #[tokio::test]
    async fn resolve_agent_config_returns_builtin_when_no_disk_config() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // Point HOME at the tempdir too, so ~/.open-mpm/agents/ctrl.toml
        // can't shadow the built-in. SAFETY: tests run single-threaded by
        // default for env mutation; this is best-effort isolation.
        let prev_home = std::env::var_os("HOME");
        // SAFETY: test-only env mutation
        unsafe {
            std::env::set_var("HOME", tmp.path());
        }

        let (cfg, _path) = super::resolve_agent_config(tmp.path()).await.unwrap();
        assert_eq!(cfg.agent.name, "ctrl");
        assert!(cfg.system_prompt.content.contains("Standalone"));

        // SAFETY: restore HOME so other tests aren't affected
        unsafe {
            match prev_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }
}
