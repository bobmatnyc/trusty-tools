# Process Model and Event Architecture

**Date**: 2026-04-25
**Status**: Design proposal — no code written

---

## 1. Current State Summary

### Binary Targets

Cargo.toml defines two binaries:

- `open-mpm` (`src/main.rs`) — the orchestrator, all execution modes
- `ompm` (`src/bin/ompm.rs`) — thin HTTP client, polls the API server

### Execution Modes (Dispatched by argv in main.rs)

| Flag / Subcommand | Mode | Description |
|---|---|---|
| _(none)_ / `--ctrl` | CTRL REPL | Interactive multi-project coordinator. Default when no flag is set. |
| `--pm` | PM one-shot | Single PM LLM round-trip on stdin, exits. Legacy backward-compat path. |
| `--agent <name>` | Sub-agent | Reads one NDJSON task from stdin, runs LLM, writes result to stdout, exits. |
| `--direct <name>` | Direct | Bypasses PM LLM, sends task straight to a named sub-agent. |
| `--workflow <name>` | Workflow | Loads a workflow JSON, runs phases sequentially/in-parallel, exits. |
| `--serve` / `--api` | API server | Starts Axum HTTP server + embedded web UI. Long-running. |
| `memory search` / `code search` | Search CLI | Queries local redb/usearch store, exits. |
| `memories <export\|import>` | Memory export | Cross-machine session sharing, exits. |
| `postmortem` | Postmortem | Runs postmortem agent, exits. |
| `agents list` / `skills list` / `inspect` | Inspection | Registry queries, exits. |
| `--reindex` / `--watch` | Indexing | Code indexing / file watching. |

### Q1: Current Process Topology

**How many processes run simultaneously today?**

In the most complex configuration (CTRL + --api server + workflow):

```
Terminal A:  open-mpm (CTRL REPL)
               └── pm_actor_task (tokio task per connected project)
                    └── run_pm_task → SubprocessAgentRunner
                         └── open-mpm --agent <name>  [OS subprocess, NDJSON IPC]

Terminal B:  open-mpm --api (Axum HTTP server)
               └── submit_task tokio::spawn
                    └── run_task → Command::new(current_exe)
                         └── open-mpm --workflow prescriptive --json
                              └── WorkflowEngine → SubprocessAgentRunner
                                   └── open-mpm --agent <name>  [OS subprocess]

Terminal C:  ompm task "..."   [thin HTTP client, exits after polling]
```

Key observations:
- There is **no coordination** between Terminal A and Terminal B. They are independent processes.
- Running `open-mpm` twice produces two completely independent process trees.
- The CTRL REPL holds PM actors as tokio tasks (not subprocesses), but those PM actors spawn sub-agents as OS subprocesses via `SubprocessAgentRunner`.
- The API server spawns `current_exe --workflow ...` as a full child process, which in turn spawns more subprocesses. It does not reuse CTRL's PM actors.

**How does the CLI talk to a running server vs. spawn one?**

- `ompm` talks to a running `open-mpm --api` server via HTTP polling (POST /api/task, GET /api/task/:id every 2s).
- There is **no detection** of a running server from `open-mpm` itself. Every invocation of `open-mpm` is a fresh process.
- No PID file, no socket probing for an already-running CTRL/server process.

**Is there a "controller" process that persists between commands?**

The CTRL REPL (`run_ctrl`) is designed to be that persistent process — it manages `PmHandle` actors in a `HashMap` and keeps them alive between `/connect` calls. However, it only lives as long as the terminal session that started it. A second invocation of `open-mpm` starts a fresh CTRL instance that knows nothing about the first.

**What happens when you run `open-mpm` twice?**

Two independent CTRL processes. Both bind the same project's Unix socket (`~/.open-mpm/sockets/<project>.sock`), the second one removes the stale socket on startup (`MessageBus::start` calls `fs::remove_file`), breaking the first. They do not discover each other.

### Q2: Current Event Model

**Are there any event queues, channels, or message buses today?**

Yes, three mechanisms exist:

1. **tokio::sync::mpsc** — CTRL → PM actor (`PmMsg::Task` / `PmMsg::Shutdown`). One channel per connected project. This is the only real async message-passing in the system.

2. **tokio::sync::broadcast** — `MessageBus` internal channel. Each project's bus broadcasts `BusEnvelope` structs to all in-process subscribers. Used for inter-project relay (when CTRL bridges a message from project A to project B's PM actor).

3. **Unix domain sockets** (`~/.open-mpm/sockets/<id>.sock`) — the inter-process layer of the message bus. Peers connect, write one NDJSON line, disconnect. The receiving bus posts it into the broadcast channel.

**How does the web UI get updates?**

Polling. `ompm` polls `GET /api/task/:id` every 2 seconds. The API server also reads `__OMPM_PROGRESS__` lines from the child subprocess's stderr and updates the in-memory `TaskStore` so the next poll returns progress events. There is **no SSE or WebSocket** today.

**How does the PM get notified when a sub-agent finishes?**

The PM blocks on `SubprocessAgentRunner::run()` which `await`s the subprocess's stdout. When the subprocess exits, the stdout pipe closes, and the IPC reader returns. There is no async notification — it is synchronous wait from the PM's perspective.

**Is there any interrupt/cancel/pause capability?**

Rudimentary only:
- `ProcessTracker` persists PIDs and can `SIGTERM` → `SIGKILL` on `shutdown_all()`. Called at startup cleanup, not wired to user-facing cancel.
- `Ctrl::shutdown_all()` sends `PmMsg::Shutdown` to each PM actor with a 5s timeout.
- No user-facing `/cancel <task-id>` command. No interrupt propagation from the web UI.

### Gaps Identified

1. **No singleton enforcement.** Two invocations of `open-mpm` do not detect each other; they race on the Unix socket.
2. **No CLI→controller IPC.** Running `open-mpm` as a CLI command against an already-running CTRL is not implemented. You can only use `ompm` against the HTTP API server.
3. **No real-time push.** The web UI is polling-only with 2s latency.
4. **Sub-agent spawning is heavy.** Every delegation spawns a new `open-mpm --agent` process, which re-initializes embedder, skill registries, redb store, etc.
5. **Cancel is unimplemented** at the user-facing level.
6. **CTRL and the API server are disjoint.** CTRL's PM actors and the API server's background tasks cannot see each other.

---

## 2. Target Architecture Diagram

```
open-mpm (single binary, single long-running process)
├── Startup: detect running controller via socket probe
│     ├── Found:  forward argv as CLI request over Unix socket, print reply, exit
│     └── Not found: become the controller (bind socket, start everything)
│
├── Controller Actor (persistent tokio task)
│     ├── Owns: session table, PM handles, shutdown signal
│     ├── Listens on: Unix socket (CLI commands) + internal event bus subscriber
│     └── Manages PM instances:
│           └── PmHandle (one tokio task per project per active session)
│                 ├── Calls LLM (async-openai / Anthropic native)
│                 ├── Uses AgentRunner (tokio-task-based, NOT subprocess)
│                 └── Emits events to EventBus
│
├── API Server (Axum, same tokio runtime)
│     ├── POST /api/task   → enqueue into Controller's task queue
│     ├── GET  /api/task/:id → read from task store
│     ├── GET  /api/events  → SSE stream from EventBus broadcast channel
│     ├── GET  /api/tasks   → list recent tasks
│     └── GET  /api/health  → liveness check
│
├── Event Bus (tokio::sync::broadcast)
│     ├── Publishers: PM tasks, agent runners, controller
│     └── Subscribers: API SSE handler, controller monitor, audit logger
│
├── CLI Client mode (when controller already running)
│     ├── Connects to Unix socket
│     ├── Sends one JSON command (task / cancel / status)
│     └── Streams replies until done, exits
│
└── Sub-Agents (hybrid model — see Q4 below)
      ├── Simple agents: tokio task (fast, shared embedder/registry)
      └── Heavy/isolated agents: subprocess with NDJSON IPC (crash isolation)
```

---

## 3. IPC Mechanism Recommendation: CLI → Controller

**Recommendation: Unix domain socket with NDJSON framing**

The project already has NDJSON framing (the `bus` module) and Unix socket infrastructure (`~/.open-mpm/sockets/<project>.sock`). The controller socket path should be well-known and project-scoped:

```
~/.open-mpm/sockets/<project-id>.ctrl.sock
```

The existing `.sock` files are for inter-project messaging. A separate `.ctrl.sock` file is the controller's command port.

**Protocol:**

CLI → Controller (one JSON line):
```json
{"type": "task", "id": "<uuid>", "text": "...", "workflow": "prescriptive", "project_path": "/abs/path"}
{"type": "cancel", "id": "<task-uuid>"}
{"type": "status"}
{"type": "attach", "session_id": "<uuid>"}
```

Controller → CLI (one or more JSON lines, connection stays open until terminal status):
```json
{"type": "progress", "id": "<task-uuid>", "phase": "code", "status": "running"}
{"type": "progress", "id": "<task-uuid>", "phase": "code", "status": "done"}
{"type": "result", "id": "<task-uuid>", "status": "success", "content": "..."}
```

**Why Unix socket over HTTP:**

| Criterion | Unix Socket | HTTP (localhost) |
|---|---|---|
| Already in codebase | Yes (bus module) | Yes (Axum server) |
| Zero config | Yes (path-based) | Requires port negotiation |
| Streaming replies | Natural (connection stays open) | Requires SSE or chunked response |
| Auth | File permissions (chmod 600) | Token header |
| Latency | ~10µs | ~1ms+ |
| Cross-machine | No (intentional) | Yes (if desired) |
| Client complexity | minimal (write/read lines) | HTTP client required |

Unix socket wins for the CLI→controller use case. The HTTP API remains for web UI and cross-machine `ompm` access — those are different clients with different needs.

**Startup detection:**

```
fn find_or_start_controller(project: &Path) -> ControllerHandle {
    let sock = ctrl_socket_path(project);
    if UnixStream::connect(&sock).is_ok() {
        return ControllerHandle::Remote(sock);   // controller already running
    }
    start_controller_in_process(project);        // we are the controller
    return ControllerHandle::Local;
}
```

---

## 4. Agent Model Recommendation: Tokio Tasks vs. Subprocesses vs. Hybrid

**Recommendation: Hybrid, with a clear split criterion**

| Agent type | Model | Rationale |
|---|---|---|
| PM orchestrator | tokio task | Already done in CTRL (PmHandle). Fast, no re-init overhead. |
| Simple sub-agents (research, docs, qa) | tokio task | No filesystem writes, bounded memory, crash loss acceptable |
| File-writing agents (engineer, python-engineer) | subprocess | Writes to disk; crash mid-write is safer to isolate. Also: these already use NDJSON IPC cleanly. |
| Shell/ops agents (local-ops-agent) | subprocess | Executes arbitrary shell; must be isolated from the controller process |
| Claude Code runner | subprocess | Already a subprocess (invokes `claude` CLI). Keep as-is. |

**Task-based agent implementation (for simple agents):**

```rust
// Instead of spawning `open-mpm --agent research-agent`, run in-process:
async fn run_agent_task(cfg: AgentConfig, task: IpcMessage) -> Result<IpcMessage> {
    let client = llm::create_client()?;
    // shared registry/embedder via Arc — no re-init
    let result = run_agent_loop(&client, &cfg, task).await?;
    Ok(result)
}
```

This eliminates per-agent startup cost (embedder model load is 2-3s on first call) and allows shared `Arc<SkillRegistry>` and `Arc<CodeStore>` across all agents in the same session.

**Crash isolation rule:** Any agent that calls `ShellExecTool` or `WriteFileTool` MUST be a subprocess. This keeps the controller process's state clean if the agent panics or is OOM-killed.

---

## 5. Event Bus Design

### Channel Types

```
EventBus = tokio::sync::broadcast::Sender<Event>
capacity  = 1024 (enough for a busy workflow with 20 phases × 50 events each)
```

The broadcast model (vs. mpsc) is correct here: multiple subscribers (SSE handler, audit log, controller monitor) all need the same events independently.

### Event Taxonomy

```rust
#[derive(Debug, Clone, Serialize)]
pub enum Event {
    // Session lifecycle
    SessionStarted   { session_id: Uuid, project: PathBuf, task: String },
    SessionCompleted { session_id: Uuid, status: Status, cost_usd: f64 },
    SessionCancelled { session_id: Uuid },

    // PM orchestration
    PmThinking       { session_id: Uuid, partial: String },     // streaming tokens
    PmDelegating     { session_id: Uuid, agent: String, task: String },

    // Agent lifecycle
    AgentSpawned     { session_id: Uuid, agent: String, agent_id: Uuid },
    AgentMessage     { session_id: Uuid, agent_id: Uuid, role: Role, content: String },
    AgentCompleted   { session_id: Uuid, agent_id: Uuid, status: Status },
    AgentFailed      { session_id: Uuid, agent_id: Uuid, error: String },

    // Tool calls (observable but not blocking)
    ToolCalled       { session_id: Uuid, agent_id: Uuid, tool: String, input: Value },
    ToolResult       { session_id: Uuid, agent_id: Uuid, tool: String, output: String },

    // Workflow phases (for progress bar)
    PhaseStarted     { session_id: Uuid, phase: String, phase_index: usize, total: usize },
    PhaseCompleted   { session_id: Uuid, phase: String, status: Status },

    // System health
    MemoryPressure   { used_mb: u64, limit_mb: u64 },
    SubprocessExited { pid: u32, agent: String, exit_code: i32 },
}
```

### Subscriber Map

| Subscriber | Events consumed | Action |
|---|---|---|
| SSE handler (`GET /api/events`) | all | Serialize to `data: ...\n\n` and write to response stream |
| Audit logger | SessionStarted, SessionCompleted, AgentFailed | Append to `~/.open-mpm/sessions/events.jsonl` |
| Controller monitor | AgentFailed, SubprocessExited | Trigger retry logic or alert user |
| Task store updater | PhaseStarted, PhaseCompleted, SessionCompleted | Update in-memory `TaskStore` for `GET /api/task/:id` polling |
| CTRL REPL printer | SessionCompleted, PmDelegating | Print status lines to terminal |
| ProcessTracker | AgentSpawned (subprocess mode), SubprocessExited | Register/deregister PID |

### Real-time web UI updates

Replace polling with SSE:

```
GET /api/events?session_id=<uuid>   → text/event-stream
```

The SSE handler subscribes to the EventBus broadcast channel and filters by `session_id`. The browser's `EventSource` API needs zero polling code. For general dashboard updates (all sessions), omit the filter.

The existing `__OMPM_PROGRESS__` stderr-scraping hack in `run_task` is replaced by events emitted natively from the workflow engine.

---

## 6. Process Lifecycle Design

### Startup Sequence

```
open-mpm invoked
    │
    ├─ Probe ctrl socket (50ms timeout)
    │     ├─ Connected: CLI client mode
    │     │     ├─ Write command as JSON line
    │     │     ├─ Stream replies to stdout until {type:result}
    │     │     └─ Exit
    │     │
    │     └─ Not connected: Controller mode
    │           ├─ Load .env.local / tracing
    │           ├─ Bump build counter
    │           ├─ Clean stale processes (ProcessTracker::cleanup_stale)
    │           ├─ Create shared singletons:
    │           │     Arc<SkillRegistry>, Arc<AgentRegistry>, Arc<CodeStore>
    │           │     Arc<EventBus>
    │           ├─ Bind ctrl socket (~/.open-mpm/sockets/<project>.ctrl.sock)
    │           ├─ Start CLI command listener (tokio task)
    │           ├─ Start Axum API server (tokio task)
    │           ├─ Start EventBus audit logger (tokio task)
    │           └─ Enter CTRL REPL (main task)
```

### Subsequent Invocations

```
open-mpm "write a Python web scraper"
    │
    └─ CLI client mode:
          ├─ Connect to ctrl socket
          ├─ Send: {"type":"task", "text":"write a Python web scraper",
          │          "project_path":"/Users/masa/myproject"}
          ├─ Stream: progress events → print to terminal
          └─ Stream: result → print, exit 0
```

### Shutdown

```
SIGINT/SIGTERM received by controller
    │
    ├─ Broadcast shutdown signal to all PM actors
    ├─ Wait for running LLM calls: 30s grace period
    │     ├─ Calls that finish: captured normally
    │     └─ Calls that don't: cancelled via tokio task abort
    ├─ SIGTERM all subprocess agents (ProcessTracker::shutdown_all)
    │     5s then SIGKILL stragglers
    ├─ Flush audit log
    ├─ Save skill effectiveness index
    ├─ Remove ctrl socket file
    └─ Exit 0
```

**What happens to running LLM calls on shutdown?**

Recommended: 30-second grace period for in-flight LLM calls. If the call completes within that window, capture the result and save the session record. If not, cancel the tokio task (the HTTP connection is dropped, OpenRouter/Anthropic will time out on their end), and write a partial session record with `status: "cancelled"`. Do not attempt to resume mid-generation — the model state is in the remote API, not locally.

### Attach to Running Session

```
open-mpm attach <session-id>
    │
    └─ CLI client mode:
          ├─ Connect to ctrl socket
          ├─ Send: {"type":"attach", "session_id":"<uuid>"}
          ├─ Controller subscribes to EventBus filtered by session_id
          └─ Stream: all events for that session → print until SessionCompleted
```

This is the "tail -f" equivalent for a running LLM task.

---

## 7. Migration Path

**Smallest step that unblocks CLI→controller→PM flow:**

### Step 1: Add a controller socket listener (2-3 days)

Extend `run_ctrl` to also bind a Unix socket and accept CLI commands alongside the stdin REPL. This is purely additive — existing REPL behavior unchanged.

```rust
// In run_ctrl, alongside the stdin reader:
tokio::spawn(ctrl_socket_listener(
    ctrl_socket_path(&project_root),
    ctrl_tx.clone(),  // mpsc channel into the ctrl event loop
));
```

The listener reads one JSON command per connection, dispatches it via the same `handle_command` / `dispatch_task` path, streams replies back.

### Step 2: Add startup detection (1 day)

In `main.rs`, before entering any mode: probe the ctrl socket. If connectable, forward argv as a CLI command and exit. This makes every invocation of `open-mpm` from the CLI automatically route through the running controller.

### Step 3: Wire EventBus to SSE (2 days)

Add `GET /api/events` SSE route to the existing Axum server. Subscribe to a `broadcast::Receiver<Event>` and forward events. Replace the `__OMPM_PROGRESS__` stderr scraper in `run_task` with native event emission.

### Step 4: Convert research-agent to tokio task (1-2 days)

Replace `SubprocessAgentRunner` for read-only agents with an in-process runner. This eliminates the 2-3s embedder re-init per delegation and makes streaming LLM tokens trivial (just emit `PmThinking` events to the bus).

### Step 5: Cancel support (1 day)

Store `JoinHandle` / `tokio::task::AbortHandle` in `PmHandle`. Wire `{"type":"cancel"}` CLI command to call `abort_handle.abort()` and propagate `SIGTERM` to any subprocess sub-agents that PM had spawned.

**Recommended first PR:** Step 1 + Step 2 together. They are the minimal viable "CLI → controller" change and produce immediately visible UX improvement (running `open-mpm "do X"` while another `open-mpm` is already running routes to the existing controller instead of starting a second one).

---

## 8. Risk Areas

### R1: Socket file lifecycle under crashes

If the controller is hard-killed (`kill -9`) the ctrl socket file remains. The next invocation probes it (connect fails → timeout) and proceeds to become the new controller, removing the stale socket. The 50ms probe timeout is critical — if probing hangs, every invocation delays by that amount. Use `connect_timeout` or run the probe on a short tokio timer.

**Mitigation:** Use `UnixStream::connect` inside `tokio::time::timeout(Duration::from_millis(50), ...)`.

### R2: Shared mutable state between tokio tasks

Moving agents from subprocesses to tokio tasks introduces shared-memory concurrency hazards. The embedder (`FastEmbedder`), `CodeStore`, and `SkillRegistry` must be `Arc<T>` (read-heavy, wrapping in `RwLock` for rare writes). Any state that is today mutated per-agent-run (session history, memory store writes) needs `Arc<Mutex<T>>` or dedicated actor tasks.

**Mitigation:** Audit each agent's mutable state before converting it from subprocess to tokio task. Convert only stateless or read-only agents first.

### R3: Embedder / fastembed initialization in shared process

`FastEmbedder::new()` loads a ~80MB ONNX model. In subprocess mode this is paid once per agent call (expensive). In shared-task mode it's paid once total (cheap), but the model occupies memory for the process lifetime. On machines with <4GB RAM this could be a problem if multiple large models are loaded simultaneously (embedder + LLM context + redb cache).

**Mitigation:** Lazy-initialize the embedder behind `OnceLock<FastEmbedder>` at the controller level. Only one embedder instance regardless of how many agents run.

### R4: LLM call cancellation semantics

Cancelling a tokio task that is `await`ing an HTTP response (to OpenRouter) drops the future but does not send an explicit cancel to the API. The remote API will time out naturally. This is acceptable for cost (you pay for tokens generated, not connection duration) but means a cancelled task's partial response is lost.

**Mitigation:** Store partial streaming tokens in the EventBus / session record so the user can see what was generated before cancel. This requires switching from `chat()` (blocking) to streaming response consumption.

### R5: Backward compatibility of the `ompm` thin client

`ompm` currently polls HTTP. If the API server is bundled into the controller process (always running), `ompm` continues to work unchanged. If someone runs `ompm` against a controller that was started without `--api`, the HTTP port won't be bound. Recommendation: always start the API server when the controller starts (on a configurable port, defaulting to 8080), removing the `--api` flag distinction.

**Mitigation:** Make the Axum server mandatory in controller mode. The `--api` flag becomes a no-op (or is removed). Port can still be customized.

### R6: Multi-project CTRL state across restarts

Today, `Ctrl.pms` (the PM handle map) is in-process memory only. If the controller restarts, all PM sessions are lost and projects must be re-connected. The session registry (`session_registry.rs`) records run IDs but not live PM state.

**Mitigation:** On controller startup, read `ProjectRegistry` and offer to auto-reconnect to projects that had active sessions at last shutdown. This is a UX improvement that can come after the socket IPC work.
