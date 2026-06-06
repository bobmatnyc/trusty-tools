# REPL Internals & Debugger Design

**Date**: 2026-04-29  
**Purpose**: Architecture reference for building a REPL debugger/inspector

---

## 1. How the REPL Runs

### Binary & Launch Path

The single binary is `open-mpm` (entry: `src/main.rs`). When invoked with no
mode flags and stdin is a TTY, the startup sequence at `main.rs:642-662` is:

1. **Probe the controller socket** (`ctrl::CtrlSocket::probe_default`) with a
   50ms timeout. If a controller is already running and the user passed
   positional argv, forward the task to it and exit — the REPL does NOT start.
2. **Spawn the controller headless** (`ctrl::run_ctrl_headless`) in a
   `tokio::spawn` background task. This binds the Unix socket listener.
3. **Sleep 250ms** to let the controller bind before the REPL probes it.
4. **Load the user profile** (`identity::user_profile::UserProfile::load`).
5. **Construct `OpenMpmRepl`** (`repl::OpenMpmRepl::new`) and call
   `repl.run().await`.
6. On exit: `ctrl_handle.abort()` — the controller background task is killed.

So at runtime there are two concurrent tasks in the same process:
- The controller (`run_ctrl_headless`) accepting socket connections.
- The REPL (`OpenMpmRepl::run`) blocking on `spawn_blocking` → `InputBox::read_line`.

### Files Involved

| File | Role |
|---|---|
| `src/main.rs:642-661` | Launch sequence, REPL/controller interleaving |
| `src/repl/mod.rs` | `OpenMpmRepl` struct + REPL loop |
| `src/repl/input.rs` | `InputBox` — raw-mode terminal input, key handling |
| `src/repl/status_bar.rs` | `StatusBar` — rendered to stderr after each task |
| `src/repl/event_display.rs` | `format_event` — converts `Event` to styled terminal lines |
| `src/ctrl/mod.rs` | `run_ctrl_headless`, `run_pm_task_with_history`, socket handler |
| `src/ctrl/socket.rs` | `CtrlSocket`, socket path helpers |
| `src/events.rs` | Global `broadcast` event bus |

---

## 2. REPL State

`OpenMpmRepl` (`src/repl/mod.rs:38-73`) carries:

| Field | Type | Description |
|---|---|---|
| `user` | `Option<UserProfile>` | Loaded from `~/.open-mpm/user.toml` |
| `project_name` | `String` | Display label; starts as `"ctrl"`, changes on `/connect` |
| `socket_path` | `PathBuf` | `~/.open-mpm/sockets/<project_id>.ctrl.sock` |
| `history_path` | `PathBuf` | `~/.open-mpm/repl_history.txt` (on-disk input history) |
| `project_dir` | `PathBuf` | Resolved project root; cwd at startup, updated by `/connect` |
| `agents_dir` | `PathBuf` | `<project_dir>/.open-mpm/agents/` |
| `skills_dir` | `PathBuf` | `<project_dir>/.open-mpm/skills/` |
| `session_id` | `String` | 8-char UUID prefix, fixed for session lifetime |
| `git_branch` | `Option<String>` | Captured once at startup via `git branch --show-current` |
| `session_start` | `Instant` | Wall-clock anchor for elapsed display |
| `status_bar` | `StatusBar` | Model/agent/token/elapsed display state |
| `conversation_history` | `Vec<ConversationTurn>` | Bounded to `MAX_HISTORY_TURNS = 20` turns |

`StatusBar` (`src/repl/status_bar.rs:53-60`) carries:

| Field | Type | Description |
|---|---|---|
| `model` | `String` | Hardcoded to `"anthropic/claude-sonnet-4-6"` at construction |
| `agent` | `Option<String>` | Active sub-agent name (not currently updated by REPL loop) |
| `tokens_in` | `u64` | Input token counter (not currently wired to LLM responses) |
| `tokens_out` | `u64` | Output token counter (not currently wired) |
| `session_start` | `Instant` | Copy of `OpenMpmRepl::session_start` |
| `config` | `StatusBarConfig` | Toggle flags for which segments render |

**Important**: `tokens_in`/`tokens_out` in `StatusBar` are currently zero — there
is no active wiring from LLM call sites back to the status bar fields. The bar
shows elapsed time and model name but not live token counts.

`InputBox` (`src/repl/input.rs:51-56`) carries:

| Field | Type | Description |
|---|---|---|
| `project_label` | `String` | Shown in the box top border |
| `history` | `Vec<String>` | In-memory up/down history; populated from disk at startup |

---

## 3. Output Mechanisms

All REPL output goes to **stderr or stdout** — never tracing. The split is intentional:

| Output path | Where | Content |
|---|---|---|
| Welcome banner | **stderr** (`eprintln!`) | `print_banner()` in `mod.rs:233-244` |
| Slash command results | **stdout** (`println!`) | `/help`, `/agents`, `/status`, etc. |
| Error messages | **stderr** (`eprintln!`) | All `Err` branches in the REPL loop |
| Forwarding indicator | **stderr** | `"→ Forwarding to controller…"` before each task |
| Event stream | **stdout** (`println!`) | `format_event` output in `event_display.rs` |
| Task response | **stdout** | Final `\n{response}\n` after task completes |
| Status bar | **stderr** | `StatusBar::render()` draws to last terminal row |
| InputBox UI | **stdout** | crossterm cursor/draw ops via `queue!/execute!` |

**Tracing**: `RUST_LOG` (or `OPEN_MPM_LOG`) controls the tracing filter.
In interactive mode the default level is `warn`, so tracing output is nearly
silent unless the user sets `RUST_LOG=debug` or higher. All tracing output
goes to stderr (configured at `main.rs:267`).

**`events::emit` vs `events::publish`**: `emit` calls `publish` AND also writes
to stderr with the prefix `__OMPM_EVENT__ <json>\n`. This is used by workflow
subprocesses so a parent API server can relay events. In the REPL context,
the relay is via the broadcast channel (`events::subscribe` / `events::bus()`).

---

## 4. Socket / IPC Interface

### Socket Location

```
~/.open-mpm/sockets/<project_id>.ctrl.sock
```

`project_id` is the sanitized basename of the project directory
(`src/ctrl/socket.rs:56-62`). For example, `/Users/masa/Projects/open-mpm`
produces `open-mpm`, so the socket lives at
`~/.open-mpm/sockets/open-mpm.ctrl.sock`.

The bus socket (inter-project messaging) uses a different suffix:
`<id>.sock` vs `<id>.ctrl.sock` — they share the directory but don't collide.

### Wire Protocol (NDJSON over Unix socket)

The controller accepts exactly **one NDJSON line per connection**, then streams
replies until it emits `done` or `error`. Implemented in
`ctrl::handle_socket_connection` (`src/ctrl/mod.rs:892-1073`).

**Client sends (one line)**:

```jsonc
// Task dispatch
{"type": "task", "id": "<uuid>", "text": "<task>", "cwd": "<path>",
 "history": [{"user": "...", "assistant": "..."}, ...]}

// Liveness probe
{"type": "status", "id": "<uuid>"}

// Graceful shutdown (acknowledged but not yet implemented)
{"type": "shutdown", "id": "<uuid>"}
```

**Controller streams back (multiple lines)**:

```jsonc
// Progress output
{"type": "output", "id": "<uuid>", "text": "<string>"}

// Final success
{"type": "done", "id": "<uuid>", "status": "success"}

// Error
{"type": "error", "id": "<uuid>", "error": "<message>"}
```

### In-Process Fallback

`OpenMpmRepl::attempt_forward` (`mod.rs:449-478`) first probes the socket with
50ms timeout. If no controller is listening, it calls
`ctrl::run_pm_task_with_history` directly in the same process — same code path,
no socket hop.

---

## 5. REPL Commands

All slash commands are handled in `try_handle_slash` (`mod.rs:263-378`):

| Command | Arg | Returns / Side-effect |
|---|---|---|
| `/help` | — | Prints command reference to stdout |
| `/exit`, `/quit` | — | Returns `Ok(false)` → REPL exits |
| `/clear` | — | ANSI clear screen; clears `conversation_history` |
| `/agents` | — | Lists agent TOML files from `agents_dir` |
| `/skills` | — | Lists `.md` files from `skills_dir` |
| `/memories [query]` | optional query | Spawns `open-mpm memories search [query]` subprocess |
| `/status` | — | Sends `{"type":"status"}` over socket; prints controller pid |
| `/session` | — | Prints `project_name` and `socket_path` |
| `/connect <path>`, `/cd <path>` | path | Updates `project_dir`, `project_name`, `socket_path`, `agents_dir`, `skills_dir`; clears `conversation_history` |
| `/version` | — | Prints `BuildInfo::load_and_increment()` |
| `/projects` | — | Loads `~/.open-mpm/projects.json` via `ProjectRegistry::load()` |
| `/log [N]` | optional N (default 20) | Tails `<project_dir>/docs/performance/runs.log` |
| `/run <file>` | path | Reads file, calls `forward_task` with its content |
| `/history [N]` | optional N (default 10) | Tails `~/.open-mpm/repl_history.txt` |

Non-slash input → `forward_task` → socket probe → controller or in-process
`run_pm_task_with_history`.

---

## 6. What a REPL Debugger Needs to Observe

### Interesting State to Inspect

1. **`conversation_history`** — the current multi-turn context window (up to 20
   turns). This is the most important piece of mutable state: it determines what
   the LLM sees. Currently not exposed anywhere outside the REPL struct.

2. **`project_name` / `project_dir` / `socket_path`** — which project is
   connected and where the socket lives. Changes on `/connect`.

3. **`status_bar.model` / `status_bar.agent`** — active model and agent name.
   `agent` is currently never set (always `None`) — a debugger could monitor
   events to track the last delegated agent name.

4. **Event bus stream** — the `tokio::sync::broadcast` channel at
   `events::EVENT_BUS`. A debugger can call `events::subscribe()` to get a
   `Receiver<Event>` and observe all events in real time without modifying any
   call sites.

5. **Socket traffic** — the NDJSON lines written to/from the controller socket.
   A proxy (`UnixStream` man-in-the-middle) could tap this.

6. **`history_path`** — on-disk REPL input history appended after every Enter.

7. **`session_id`** — 8-char UUID present on every call but not surfaced in UI.
   All events carry a `session_id` that correlates to the task submission.

### Interesting Output to Tap

- **Event bus**: subscribe via `events::subscribe()` — zero-copy access to all
  `PmThinking`, `AgentMessage`, `ToolCalled`, `SessionDone`, etc. events in
  real time.
- **Stderr relay prefix**: lines starting with `__OMPM_EVENT__ ` (see
  `events::EVENT_LINE_PREFIX`) carry serialized JSON events from subprocesses.
- **Tracing**: set `RUST_LOG=debug` to see internal call paths, LLM request
  durations, and IPC message routing.

### Key Attach Points for a Debugger

| What to observe | How to attach |
|---|---|
| All real-time events | `events::subscribe()` in a sibling task |
| Socket request/response | Proxy or `strace`/`lldb` on the Unix socket |
| Conversation history snapshot | Add a `/debug-state` slash command that serializes `self.conversation_history` |
| Token usage | Wire `LlmResponded { completion_tokens }` event into `StatusBar::tokens_out` |
| Per-turn latency | Timestamps from `LlmRequested` / `LlmResponded` events |
| Active agent | Last `PmDelegating { agent }` event observed on the bus |
| Current project | Read `OpenMpmRepl::project_name` / `project_dir` (requires struct access) |

---

## 7. Existing Debug/Inspect Hooks

There are **no dedicated debug hooks** in the current REPL. What does exist:

1. **`events` broadcast bus** (`src/events.rs`) — the richest observability
   surface. Any code can call `events::subscribe()` to receive a live feed of
   typed events. The REPL itself uses this via `spawn_event_relay` in
   `mod.rs:703-725`.

2. **`__OMPM_EVENT__` stderr relay** (`events::emit` vs `events::publish`) —
   subprocess event relay protocol. Not used by the REPL loop itself, but
   available to workflow child processes.

3. **`tracing`** — structured debug logs. Not surfaced in the REPL by default
   (`warn` level). Set `RUST_LOG=debug` to enable.

4. **`/session` command** — prints `session_id` + socket path (very minimal).

5. **`/status` command** — pings the controller socket and prints PID.

6. **`/log [N]`** — tails the perf runs log (workflow performance, not REPL).

7. **`/history [N]`** — tails the on-disk input history file.

8. **`StatusBar`** — token/elapsed display, but token counts are not wired.

---

## Architecture Diagram

```
main() [main.rs:642]
  │
  ├── tokio::spawn(run_ctrl_headless)   ← binds ~/.open-mpm/sockets/<id>.ctrl.sock
  │     └── spawn_socket_listener       ← one task per inbound connection
  │           └── handle_socket_connection  ← parses NDJSON, calls run_pm_task_with_history
  │
  └── OpenMpmRepl::run() [mod.rs:162]
        │
        ├── InputBox::read_line()        ← blocking, in spawn_blocking, raw mode
        │     └── handle_key / draw      ← crossterm, stdout
        │
        ├── try_handle_slash()           ← local command dispatch, stdout/stderr
        │
        └── forward_task()              ← non-slash input
              │
              ├── spawn_event_relay      ← subscribes to EVENT_BUS broadcast
              │     └── format_event → println!   ← stdout
              │
              └── attempt_forward()
                    ├── CtrlSocket::probe_default  ← 50ms timeout
                    │     └── [success] forward_to_controller  ← socket NDJSON
                    └── [failure] run_pm_task_with_history     ← in-process

EVENT_BUS (OnceLock<broadcast::Sender<Event>>) [events.rs:247]
  └── any subscriber: events::subscribe() → Receiver<Event>
        ← PmThinking, PmDelegating, AgentSpawned, AgentMessage,
           ToolCalled, SessionStarted, SessionDone, LlmRequested,
           LlmResponded, PhaseStarted, PhaseDone, ...
```

---

## Recommendations for a Debugger

**Minimum viable debugger** (no source changes required):

1. Subscribe to `events::subscribe()` from a sibling tokio task — this gives
   live event feeds with session IDs, agent names, tool calls, and LLM latency.
2. Open a second connection to the `.ctrl.sock` and send `{"type": "status"}`
   periodically to confirm controller liveness.
3. Watch `~/.open-mpm/repl_history.txt` (inotify/kqueue) for user input entries.

**Richer debugger** (small source additions):

1. Add a `/debug-state` slash command in `try_handle_slash` that serializes
   the REPL struct fields to a JSON file or stdout.
2. Wire `LlmResponded { completion_tokens, latency_ms }` event into
   `StatusBar::tokens_out` and `tokens_in` so the bar shows live usage.
3. Add a `/inspect` socket command type in `handle_socket_connection` that
   returns the current conversation history length and last-turn preview.
4. Expose `conversation_history` and `project_dir` as a periodic broadcast
   event (new `ReplStateSnapshot` variant on `Event`) for external consumers.
