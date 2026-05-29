# Tauri Chat Interface Design Brief

**Date**: 2026-04-24
**Scope**: Design brief for a Tauri chat interface that talks to open-mpm.
**Reference**: ai-commander (`/Users/masa/Projects/ai-commander`) as the GUI template.

---

## Part 1: ai-commander Reference Stack

### Project Layout

ai-commander is a Rust workspace with dedicated crates:

| Crate | Purpose |
|---|---|
| `crates/commander-gui/` | Tauri 2 desktop app (Svelte frontend + Rust backend) |
| `crates/commander-api/` | Axum REST API, port 9876 |
| `crates/commander-daemon/` | Background services (idle monitor, health, message polling) |
| `crates/commander-adapters/` | Runtime adapters (ClaudeCode, MPM, Auggie, etc.) |
| `crates/commander-core/` | Shared logic |

### Frontend Stack

- **Framework**: Svelte 4 (not SvelteKit — plain Vite + `@sveltejs/vite-plugin-svelte`)
- **Language**: TypeScript
- **Build**: Vite 5
- **Styling**: Tailwind CSS 3 + PostCSS
- **Icons**: lucide-svelte
- **State management**: Svelte writable stores (no Redux/Zustand)
- **Tauri API**: `@tauri-apps/api` v2

### Tauri Version

Tauri 2 (`tauri = { version = "2" }`). Uses `tauri::Emitter`, `tauri::Manager`, `tauri::generate_handler![]`.

### Tauri Backend (Rust)

`crates/commander-gui/src/`:
- `main.rs` — app setup, spawns REST API + daemon in background, registers all commands
- `commands.rs` — all `#[tauri::command]` handlers
- `events.rs` — background polling loop that captures tmux output and emits Tauri events
- `state.rs` — `GuiState` struct managed via `app.manage(state)`

### Key Tauri Commands Registered

```rust
commands::list_sessions
commands::connect_session / disconnect_session / stop_session
commands::send_message / send_message_streaming
commands::create_session
commands::list_project_directories
commands::list_adapters
commands::interpret_session
commands::capture_session_output
commands::list_processes / kill_stale_processes
commands::rename_session
commands::get_github_stats
```

### Frontend Component Structure

```
App.svelte              — root, theme toggle, service status checks
  SessionList.svelte    — left sidebar: list of sessions, connect/disconnect
  ChatView.svelte       — main panel: message history, raw mode, pagination
  InputArea.svelte      — bottom bar: text input, send button, slash commands
  CreateSessionModal.svelte
  SettingsModal.svelte
  DashboardView.svelte
  ProcessMonitorPanel.svelte
```

### IPC Pattern: Tauri invoke vs REST fallback

The codebase has an elegant dual-mode transport at `ui/src/lib/transport.ts`:

```typescript
const isTauri = '__TAURI_INTERNALS__' in window || '__TAURI__' in window;

export async function api(command, args) {
  if (isTauri) {
    return tauriInvoke(command, args);   // @tauri-apps/api/core invoke()
  }
  return fetchApi(command, args);        // REST HTTP fallback
}
```

All components import from `transport.ts` rather than directly from `@tauri-apps/api`. This lets the same UI run as both a Tauri desktop app and a web app served over REST.

### Svelte Store Design

`ui/src/lib/stores/app.ts` uses Svelte writable stores:

```typescript
sessionMessages: writable<Map<string, Message[]>>  // per-session message history
currentSession: writable<Session | null>
sessions: writable<Session[]>
messages: derived([sessionMessages, currentSession], ...)  // messages for active session
```

Message direction enum: `'sent' | 'received' | 'system'`.

Message consolidation: consecutive system messages from the same sender within 5 minutes are merged into a single bullet-block. Received messages within 30 seconds update in place (no duplicate bubbles).

### Streaming Pattern

`send_message_streaming` in `commands.rs` (line 959) shows the established approach:

1. POST to `http://localhost:7777/api/v1/sessions/{session}/messages` with `{ content, stream: true }`
2. Read SSE response as a byte stream (`resp.bytes_stream()`)
3. Parse `data: {...}` lines from the stream
4. Emit incremental Tauri events to the frontend: `app.emit("chat-event", ...)`
5. Frontend listens with `listen('chat-event', handler)` from `@tauri-apps/api/event`

SSE event types: `text` (incremental), `tool_use`, `complete`, `error`.

### Background Polling

`events::start_session_polling()` runs a 500ms polling loop per connected session. It captures tmux scrollback, hashes it for change detection, throttles LLM calls (5s min gap), and emits `session-output` / `chat-event` Tauri events.

---

## Part 2: open-mpm Controller and API

### Controller (`src/ctrl/mod.rs`)

The CTRL actor manages multiple named `PmHandle` instances keyed by canonical project path. Each `PmHandle` wraps a `tokio::mpsc` channel to a background `pm_actor_task` coroutine.

**Protocol:**
```rust
enum PmMsg {
    Task { text: String, reply: oneshot::Sender<Result<String>> },
    Shutdown,
}
```

Key operations:
- `/connect <path>` — canonicalizes path, spawns a `pm_actor_task` if new, sets as active
- `/disconnect` — clears active focus but leaves actor running
- `/status` — lists all PM sessions
- Text input — `dispatch_task(text)` sends via mpsc, awaits `oneshot` reply

The controller is a **CLI REPL** over stdin/stdout. It does NOT expose an HTTP or WebSocket API directly.

### API Server (`src/api/server.rs`)

There IS an HTTP API, built with Axum. Four routes:

| Route | Method | Description |
|---|---|---|
| `/api/health` | GET | `{ "status": "ok", "version": "..." }` |
| `/api/task` | POST | Submit a task, returns `{ "id": uuid, "status": "running" }` |
| `/api/task/:id` | GET | Poll for task result (`PmResponse` JSON) |
| `/api/tasks` | GET | List up to 20 recent responses |

**Critical implementation detail**: The HTTP server does NOT have a running PM/CTRL in memory. When `POST /api/task` is called, it **re-spawns the current binary** as a subprocess:

```rust
let mut cmd = Command::new(current_exe()?);
cmd.arg("--workflow").arg(workflow).arg("--json").arg("--task").arg(&req.task);
```

This means:
- The API server is a thin wrapper that self-respawns the binary per task
- No persistent PM session lives inside the HTTP server
- Responses are poll-based (submit → get UUID → poll `/api/task/:id`)
- No streaming output from the HTTP server currently

### Response Type (`src/api/types.rs`)

```rust
pub struct PmResponse {
    pub id: String,
    pub timestamp: String,        // ISO8601
    pub response_type: PmResponseType,  // workflow_result | agent_response | task_submitted | error
    pub status: PmStatus,         // success | partial | failed | running
    pub narrative: String,        // human-readable output
    pub metadata: PmMetadata,     // tokens, cost, timing
    pub phases: Vec<PhaseResult>,
    pub files_modified: Vec<String>,
    pub errors: Vec<String>,
}
```

### Binary Execution Modes

```
open-mpm                              → CTRL interactive REPL
open-mpm --workflow <name> --task ... → single workflow run, exits
open-mpm --direct <agent> --task ...  → single agent call, exits
open-mpm --agent <name>               → sub-agent mode (NDJSON over stdio)
open-mpm --api --port <n>             → HTTP API server mode (presumed)
```

### IPC Summary

There is **no WebSocket** and **no persistent HTTP session**. The two communication paths available to an external client are:

1. **HTTP REST (poll-based)**: `POST /api/task` → poll `/api/task/:id` for `PmResponse`. Simple but not streaming.

2. **Subprocess stdio (NDJSON)**: Spawn `open-mpm --workflow ... --task ...` and read its stdout. Streaming is possible by reading stdout line-by-line as the subprocess writes NDJSON.

The CTRL module is internal and not exposed over a socket today.

---

## Design Brief: open-mpm Tauri Chat Interface

### 1. Recommended Frontend Stack

Copy the ai-commander stack exactly:

| Component | Choice | Rationale |
|---|---|---|
| Framework | Svelte 4 | Proven in ai-commander, lightweight, no VDOM overhead |
| Language | TypeScript | Type safety for Tauri command signatures |
| Build | Vite 5 | Fast HMR, Tauri-compatible config already exists in reference |
| Styling | Tailwind CSS 3 | Reference already has full setup including postcss.config.js |
| Icons | lucide-svelte | Already used in reference |
| State | Svelte writable stores | Sufficient for this use case; no need for heavier solutions |
| Tauri | v2 | Match reference; `@tauri-apps/api` v2 |

Reuse the `transport.ts` dual-mode pattern to make the UI usable as a web app over the existing HTTP API without Tauri present.

### 2. How the Tauri App Should Talk to open-mpm

**Short answer**: The Tauri backend should spawn open-mpm as a subprocess per task and communicate via NDJSON stdio — OR call the existing HTTP API in poll mode.

**Option A (recommended for MVP): HTTP poll mode**

open-mpm already has a working HTTP API server. The Tauri app can:
1. Ensure `open-mpm --api` is running (spawn it on startup if not detected)
2. POST tasks to `http://localhost:<port>/api/task`
3. Poll `GET /api/task/:id` until `status != "running"`
4. Display `narrative` from `PmResponse`

Pros: No subprocess management complexity in Tauri. The API server handles the subprocess re-spawning internally.
Cons: No streaming. Responses can take 30–120s; need a progress indicator.

**Option B: Direct subprocess spawning with NDJSON stdio**

The Tauri backend spawns `open-mpm --workflow prescriptive --task "..."` directly:
- Read stdout line-by-line
- Emit Tauri events to frontend as lines arrive
- Enables streaming output display

Pros: Streaming, no need for a running server.
Cons: More complex Tauri-side code; process lifecycle management needed.

**Recommendation**: Start with Option A (HTTP poll) because the API is already tested and working. Add Option B streaming later by adding a streaming endpoint to the HTTP server (SSE from the subprocess stdout).

**Connection lifecycle**:
```
Tauri startup:
  → check GET http://localhost:<port>/api/health
  → if 404/error: spawn `open-mpm --api --port <port>` as a sidecar
  → store sidecar handle, kill on window close
```

### 3. Key Tauri Commands Needed

```rust
// Session/project management
#[tauri::command]
async fn start_pm(project_path: String, state: State<AppState>) -> Result<String, String>
// Connects to a project directory (maps to ctrl.connect())
// For HTTP mode: just records the project path for subsequent task calls

#[tauri::command]
async fn list_sessions(state: State<AppState>) -> Result<Vec<SessionInfo>, String>
// Returns known PM sessions / project paths

#[tauri::command]
async fn list_projects(state: State<AppState>) -> Result<Vec<String>, String>
// Read from ~/.open-mpm/projects.json (ProjectRegistry)

// Task dispatch
#[tauri::command]
async fn send_message(
    project_path: String,
    content: String,
    workflow: Option<String>,
    state: State<AppState>,
    app: AppHandle,
) -> Result<String, String>
// POST /api/task, poll until complete, emit progress events

#[tauri::command]
async fn get_task_status(task_id: String, state: State<AppState>) -> Result<PmResponse, String>
// GET /api/task/:id — for manual polling from frontend

#[tauri::command]
async fn list_tasks(state: State<AppState>) -> Result<Vec<PmResponse>, String>
// GET /api/tasks — recent task history

// Server management
#[tauri::command]
async fn check_api_health(state: State<AppState>) -> Result<bool, String>

#[tauri::command]
async fn ensure_api_server(port: u16, state: State<AppState>) -> Result<(), String>
// Spawn open-mpm --api if health check fails
```

Tauri events emitted from backend to frontend:

```
"task-progress"   → { task_id, message: "Running workflow..." }
"task-complete"   → PmResponse JSON
"task-error"      → { task_id, error: "..." }
"server-status"   → { running: bool, port: u16 }
```

### 4. UI Component Structure

```
App.svelte
  ├── Sidebar (left, ~280px)
  │   ├── ProjectPicker.svelte
  │   │   — text input or folder picker (tauri dialog)
  │   │   — /connect <path> button
  │   ├── SessionList.svelte
  │   │   — list of connected projects (name, path, status indicator)
  │   │   — click to switch active project
  │   └── TaskHistory.svelte
  │       — list of recent PmResponse items (status badge, timestamp)
  │       — click to view task detail
  │
  └── MainPanel (right)
      ├── ChatView.svelte
      │   — message bubbles: sent (user), received (PM narrative), system (status)
      │   — streaming/loading state while task is running
      │   — Markdown rendering for received messages
      │   — pagination (copy PAGE_SIZE=10 pattern from ai-commander)
      ├── InputArea.svelte
      │   — textarea (multi-line, Shift+Enter for newline)
      │   — Send button
      │   — workflow selector dropdown (prescriptive / direct / custom)
      │   — disabled state while task is running
      └── TaskDetailView.svelte (optional overlay/panel)
          — full PmResponse: phases, tokens, cost, files_modified
          — shown when clicking a history entry
```

### 5. Gotchas and Constraints

**From ai-commander:**

1. **Tauri v1 vs v2**: The reference uses Tauri v2. `__TAURI_INTERNALS__` is the v2 detection key; `__TAURI__` is v1. The transport.ts checks both — copy this exactly.

2. **Dual-mode transport**: Wire the `transport.ts` abstraction from day one. It avoids getting locked into Tauri-only invoke calls and enables a web UI for free.

3. **Event listener registration race**: ai-commander adds a 500ms startup delay before emitting auto-connect events so the frontend's `listen()` registration completes first. Apply the same pattern for any startup events.

4. **Background server cleanup**: `main.rs` uses `tauri::WindowEvent::Destroyed` to abort the background API server task. Copy this to prevent orphaned `open-mpm --api` processes on window close.

5. **CSP is null**: `tauri.conf.json` has `"csp": null`. This is fine for localhost-only apps but note it for future security review.

**From open-mpm:**

6. **No persistent PM session in HTTP mode**: The API server re-spawns the binary per task. There is no "session" concept at the HTTP layer — just fire-and-forget tasks with poll. The Tauri app must map "project directory" to "task context" by including the project path in each task request (via `--out-dir` or an equivalent flag).

7. **Task duration**: Workflow tasks can take 30–120 seconds. The UI must show a spinner/progress indicator and disable the input area while a task is running. Poll interval of 2–5s is appropriate.

8. **No streaming from HTTP API today**: If streaming output is a hard requirement, the open-mpm HTTP server needs a new SSE endpoint. The `run_task` function in `api/server.rs` currently awaits the entire subprocess before returning — it would need to stream subprocess stdout as SSE events.

9. **Working directory matters**: open-mpm loads `.open-mpm/agents/*.toml` relative to CWD. When the API server re-spawns itself, it inherits the server's CWD. For multi-project support, the Tauri app must either pass `--out-dir` per project or the API server needs a `project_path` field in `TaskRequest` (it already has `out_dir` but not a `project_path` for config discovery).

10. **OPENROUTER_API_KEY required**: The Tauri app must either read the key from `.env.local` in the project directory or prompt the user to set it in a settings panel. The existing API server inherits the env from its parent process.

---

## Implementation Sequence

1. Scaffold Tauri 2 project under `crates/ompm-gui/` (or a new `tauri-ui/` directory)
2. Copy Svelte + Vite + Tailwind setup from ai-commander (`package.json`, configs)
3. Port `transport.ts` dual-mode abstraction
4. Implement `ProjectPicker` + `SessionList` stores
5. Add `ensure_api_server` Tauri command to spawn/health-check open-mpm
6. Add `send_message` Tauri command with polling loop + event emission
7. Port `ChatView` + `InputArea` components from ai-commander, adapting for PmResponse shape
8. Wire `TaskHistory` sidebar from `GET /api/tasks`
9. Add `TaskDetailView` for phase breakdown and file listing from PmResponse
