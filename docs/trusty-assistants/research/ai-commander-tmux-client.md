# ai-commander tmux Client Research

**Date**: 2026-04-29  
**Project**: ~/Projects/ai-commander  
**Scope**: tmux interaction layer only (not AI/LLM components)

---

## 1. Relevant Files

All tmux logic is isolated to the `commander-tmux` crate:

| File | Purpose |
|---|---|
| `crates/commander-tmux/src/lib.rs` | Crate root, re-exports public API |
| `crates/commander-tmux/src/orchestrator.rs` | Core tmux client — all subprocess calls |
| `crates/commander-tmux/src/session.rs` | Data structures + parsing |
| `crates/commander-tmux/src/error.rs` | Error types |
| `crates/commander-tmux/Cargo.toml` | Crate dependencies |

**Consumers of commander-tmux:**

| File | Usage |
|---|---|
| `crates/commander-runtime/src/executor.rs` | Starts/stops instances, sends commands, captures output |
| `crates/commander-runtime/src/poller.rs` | Polls pane output on a timer |
| `crates/ai-commander/src/tui/app.rs` | Holds `Option<TmuxOrchestrator>` in App state |
| `crates/ai-commander/src/tui/inspect.rs` | Inspect mode: calls `capture_output` |
| `crates/ai-commander/src/tui/sessions.rs` | Session list scanning, session connection |
| `crates/commander-daemon/src/service.rs` | Daemon service (minimal usage) |

---

## 2. How It Connects to / Interacts with tmux

The implementation is a **thin subprocess wrapper** — it does not use any tmux C library or socket protocol. Every operation shells out to the `tmux` binary via `std::process::Command` (synchronous, not tokio).

**Binary discovery** (orchestrator.rs line 36-48):
```rust
Command::new("which").arg("tmux").output()
```
The path is stored in `TmuxOrchestrator.tmux_path: String`.

**All subsequent calls** use:
```rust
fn run_tmux(&self, args: &[&str]) -> Result<Output>   // line 51
fn run_tmux_checked(&self, args: &[&str]) -> Result<String>  // line 64
```
`run_tmux_checked` returns stdout as a String or wraps stderr in `TmuxError::CommandFailed`.

---

## 3. Core API / Public Interface

Defined on `TmuxOrchestrator` (orchestrator.rs):

### Availability
- `TmuxOrchestrator::new() -> Result<Self>` — verifies tmux in PATH
- `TmuxOrchestrator::is_available() -> bool` — non-failing probe

### Session Management
- `create_session(name: &str) -> Result<TmuxSession>` — calls `tmux new-session -d -s <name>`
- `create_session_in_dir(name, dir: Option<&str>) -> Result<TmuxSession>` — adds `-c <dir>`
- `destroy_session(name: &str) -> Result<()>` — calls `tmux kill-session -t <name>`
- `list_sessions() -> Result<Vec<TmuxSession>>` — parses `tmux list-sessions -F "#{session_name}:#{session_created}:#{session_group}"`
- `session_exists(name: &str) -> bool` — calls `tmux has-session -t <name>`

### Pane Management
- `create_pane(session: &str) -> Result<TmuxPane>` — calls `tmux split-window -t <session>`
- `list_panes(session: &str) -> Result<Vec<TmuxPane>>` — parses `tmux list-panes -t <session> -F "#{pane_id}:#{pane_index}:#{pane_active}:#{pane_width}:#{pane_height}"`

### I/O
- `capture_output(session, pane: Option<&str>, lines: Option<u32>) -> Result<String>` — calls `tmux capture-pane -t <target> -p [-S -<n>]`
- `send_keys(session, pane: Option<&str>, keys: &str) -> Result<()>` — calls `tmux send-keys -t <target> <keys>`
- `send_line(session, pane: Option<&str>, text: &str) -> Result<()>` — two-step: literal text via `-l` flag, then `Enter`

---

## 4. Data Structures

### `TmuxSession` (session.rs lines 9-21)
```rust
pub struct TmuxSession {
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub panes: Vec<TmuxPane>,
    pub group: Option<String>,   // #{session_group}, None if empty
}
```
Parsed from format `name:timestamp[:group]` (line 45).

### `TmuxPane` (session.rs lines 83-94)
```rust
pub struct TmuxPane {
    pub id: String,      // e.g. "%0", "%1"
    pub index: u32,
    pub active: bool,
    pub width: u32,
    pub height: u32,
}
```
Parsed from format `pane_id:pane_index:pane_active:pane_width:pane_height` (line 111).

### `RunningInstance` (executor.rs lines 20-33)
Used by the runtime layer, not in commander-tmux itself:
```rust
pub struct RunningInstance {
    pub project_id: ProjectId,
    pub session_name: String,       // derived from project.name with chars replaced
    pub adapter: Arc<dyn RuntimeAdapter>,
    pub started_at: DateTime<Utc>,
    pub last_output: Option<String>,
    pub state: ProjectState,
}
```

### `TmuxError` (error.rs)
```rust
pub enum TmuxError {
    NotFound,
    SessionNotFound(String),
    PaneNotFound(String, String),
    CommandFailed(String),
    Io(#[from] std::io::Error),
    ParseError(String),
}
```

---

## 5. Reading Pane Output

**Method**: `capture_output(session, pane, lines)` (orchestrator.rs line 252)

- Target is `"session"` (active pane) or `"session:pane_id"` (specific pane)
- tmux command: `capture-pane -t <target> -p` with optional `-S -<n>` for line count
- `-p` flag writes to stdout instead of a buffer; the output is the raw terminal text
- Returns a `String` of all captured lines
- **Note**: pane existence is validated *after* capture, not before — a quirk at lines 279-284

**Common call sites**:
- `executor.rs:314` — `capture_output(&session_name, None, Some(50))`
- `poller.rs:73` — polling loop
- `inspect.rs:31` — `capture_output(session, None, Some(200))` for inspect mode
- `sessions.rs:51,147,251` — `capture_output(&session, None, Some(50))` for adapter detection and monitoring

---

## 6. Sending Input

**Two methods**:

`send_keys(session, pane, keys)` (orchestrator.rs line 300):
- Calls `tmux send-keys -t <target> <keys>`
- Passes key names literally — callers can send `"C-c"`, `"Enter"`, etc.
- Used by `executor.rs:220` to send `"C-c"` on graceful stop

`send_line(session, pane, text)` (orchestrator.rs line 327):
- Step 1: `tmux send-keys -t <target> -l <text>` — `-l` treats text as literal (no key-name expansion)
- Step 2: `tmux send-keys -t <target> Enter` — separate call to submit
- Used by `executor.rs:165` to launch the AI tool command in the session

---

## 7. Crate Dependencies (Cargo.toml)

```toml
[dependencies]
chrono     = { workspace = true }   # version 0.4 with serde feature
thiserror  = { workspace = true }   # version 1.0
tracing    = { workspace = true }   # version 0.1

[dev-dependencies]
tempfile   = { workspace = true }   # version 3.10
```

No async runtime dependency. All `std::process::Command` calls are synchronous/blocking.

---

## 8. TUI / REPL Inspector Code

Yes — there is an **Inspect Mode** in the TUI (`crates/ai-commander/src/tui/`):

| File | Role |
|---|---|
| `inspect.rs` | Logic: `toggle_inspect_mode()`, `refresh_inspect_content()`, scroll methods |
| `ui.rs:69-124` | Renderer: `draw_inspect()` — ratatui-based terminal UI |
| `app.rs:113,171-173` | State: `view_mode: ViewMode`, `inspect_content: String`, `inspect_scroll: usize` |

**How it works**:
- `App.tmux: Option<TmuxOrchestrator>` — held in TUI app state (app.rs line 138)
- `App.sessions: HashMap<String, String>` — maps project name → tmux session name (app.rs line 144)
- `refresh_inspect_content()` calls `tmux.capture_output(session, None, Some(200))` and stores result in `inspect_content`
- The TUI refreshes every 100ms (noted in footer text)
- Scroll uses reverse indexing (0 = bottom, increasing = scrolling up)
- `ViewMode` enum: `Normal`, `Inspect`, `Sessions` (app.rs lines 109-117)
- Toggle bound to F2; exit via F2, Esc, or `q`

The render in `ui.rs:draw_inspect` uses `ratatui` with a `Block` + `Paragraph` widget, no ANSI escape stripping — raw tmux text is rendered as-is.

---

## Key Design Notes

1. **Synchronous subprocess model**: No async anywhere in commander-tmux. The runtime layer wraps calls in async methods but the tmux calls themselves block.
2. **No tmux socket/control mode**: Uses only the tmux CLI. No use of `tmux -C` (control mode) or direct Unix socket communication.
3. **Graceful degradation**: All consumer crates check `TmuxOrchestrator::is_available()` and skip tmux-dependent logic when tmux is absent.
4. **Session naming**: `executor.rs:144` derives session name from project name by replacing `[' ', '.', '/', ':']` with `"-"`.
5. **No ANSI stripping**: Captured output is returned as raw terminal content including any escape sequences tmux stores.
