# TM (Tmux Manager) — Technical Specification

**Date**: 2026-05-04  
**Status**: Draft — pre-implementation  
**Scope**: Research spec driving ticket creation and implementation  

---

## 1. Overview

The TM (Tmux Manager) extends open-mpm to natively manage tmux sessions for any AI
coding harness — not just the native PM orchestrator. It treats every tmux session
as a first-class "project session" and provides a unified REPL surface for creating,
inspecting, pausing, resuming, and killing sessions running claude-code, claude-mpm,
codex, augment-code, gemini-code, and plain shells.

The design is derived from the `commander-tmux` and `commander-adapters` crates in
`ai-commander` (Rust, `/Users/masa/Projects/ai-commander`), translated into open-mpm's
module conventions and storage layout.

---

## 2. Architecture

### 2.1 Where TM Sits in open-mpm

```
open-mpm process
├── PM Orchestrator (src/main.rs)        ← existing, unchanged
├── REPL (src/repl/)                     ← gains /tm* slash commands
├── src/tmux/                            ← NEW: raw tmux primitives
│   ├── mod.rs
│   ├── orchestrator.rs                  ← TmuxOrchestrator (sync, wraps tmux binary)
│   ├── session.rs                       ← TmuxSession, TmuxPane data structs
│   └── error.rs                         ← TmuxError, Result<T>
├── src/adapters/                        ← NEW: harness adapter plugins
│   ├── mod.rs
│   ├── traits.rs                        ← HarnessAdapter trait + detection API
│   ├── claude_mpm.rs
│   ├── claude_code.rs
│   ├── codex.rs
│   ├── augment.rs
│   ├── gemini.rs
│   ├── shell.rs
│   └── registry.rs                      ← AdapterRegistry (HashMap + detect())
└── src/tm/                              ← NEW: TM session model + commands
    ├── mod.rs
    ├── project.rs                       ← TmProject, TmSession data model
    ├── registry.rs                      ← TmSessionRegistry (JSON-backed)
    ├── manager.rs                       ← TmManager: high-level operations
    └── commands.rs                      ← /tm REPL command dispatcher
```

### 2.2 Data Model

#### TmProject

A tmux project is a directory being worked on with one or more active tmux sessions.
It is distinct from open-mpm's existing "project" concept (which is just a cwd +
agent TOML lookup). The two coexist: a PM session and a TM session can share the
same project directory.

```rust
/// A directory-rooted project that groups related tmux sessions.
pub struct TmProject {
    /// Stable identifier — UUID v4.
    pub id: String,
    /// Human label (e.g., "my-api").
    pub name: String,
    /// Absolute path of the working directory.
    pub path: PathBuf,
    /// IDs of sessions associated with this project.
    pub session_ids: Vec<String>,
    /// When the project was registered.
    pub created_at: DateTime<Utc>,
}
```

#### TmSession

Each session maps one-to-one with a tmux session and carries enough metadata to
drive adapter dispatch, lifecycle control, and the registry.

```rust
/// One tmux session managed by TM.
pub struct TmSession {
    /// Stable identifier — UUID v4.
    pub id: String,
    /// Unique session name within this open-mpm instance (also the tmux session
    /// name, must satisfy tmux naming rules: no dots, no colons).
    pub name: String,
    /// Project this session belongs to.
    pub project_id: String,
    /// Absolute path of the project directory.
    pub project_path: PathBuf,
    /// Which harness adapter drives this session.
    pub adapter_type: AdapterType,
    /// The actual tmux session name (may differ if a collision was resolved).
    pub tmux_session_name: String,
    /// Current lifecycle state.
    pub status: SessionStatus,
    /// Wall-clock time this session was created.
    pub created_at: DateTime<Utc>,
    /// Last time TM observed activity in this session.
    pub last_active: DateTime<Utc>,
    /// Notes / tags added by the user.
    pub notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AdapterType {
    ClaudeMpm,
    ClaudeCode,
    Codex,
    Augment,
    GeminiCode,
    Shell,
    OpenMpm,   // native open-mpm PM session running inside tmux
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionStatus {
    /// tmux session exists and harness is active.
    Running,
    /// Harness has been paused (e.g., /mpm-session-pause sent).
    Paused,
    /// tmux session exists but harness is idle.
    Idle,
    /// tmux session no longer exists.
    Orphaned,
    /// Intentionally killed.
    Stopped,
}
```

### 2.3 Session Lifecycle State Machine

```
           create()
  ─────────────────►  Running
                          │
             pause()      │       resume()
  Paused ◄───────────────►├──────────────► Running
                          │
             kill()       │
  Stopped ◄───────────────┘
                          │
  (tmux session vanishes) │
  Orphaned ◄──────────────┘
```

Idle is a transient sub-state of Running (harness alive but waiting for input).
Polling via `capture_output` detects idle vs. working via adapter pattern matching.

---

## 3. Tmux Primitives

`src/tmux/orchestrator.rs` is a near-direct port of the `TmuxOrchestrator` from
`ai-commander/crates/commander-tmux/src/orchestrator.rs`. It uses synchronous
`std::process::Command` (not tokio) because every individual tmux call is
sub-millisecond.

### 3.1 Core API

```rust
pub struct TmuxOrchestrator {
    tmux_path: String,
}

impl TmuxOrchestrator {
    pub fn new() -> Result<Self>;
    pub fn is_available() -> bool;

    // Session lifecycle
    pub fn create_session(&self, name: &str, dir: Option<&str>) -> Result<TmuxSession>;
    pub fn destroy_session(&self, name: &str) -> Result<()>;
    pub fn list_sessions(&self) -> Result<Vec<TmuxSession>>;
    pub fn session_exists(&self, name: &str) -> bool;

    // Pane management
    pub fn list_panes(&self, session: &str) -> Result<Vec<TmuxPane>>;
    pub fn create_pane(&self, session: &str) -> Result<TmuxPane>;

    // I/O
    pub fn capture_output(&self, session: &str, pane: Option<&str>, lines: Option<u32>) -> Result<String>;
    pub fn send_keys(&self, session: &str, pane: Option<&str>, keys: &str) -> Result<()>;
    pub fn send_line(&self, session: &str, pane: Option<&str>, text: &str) -> Result<()>;
}
```

### 3.2 Tmux Commands Used

| Operation | Tmux Command |
|---|---|
| Create detached session | `tmux new-session -d -s <name> -c <dir>` |
| Kill session | `tmux kill-session -t <name>` |
| Check session exists | `tmux has-session -t <name>` (exit code) |
| List sessions | `tmux list-sessions -F "#{session_name}:#{session_created}:#{session_group}"` |
| List panes | `tmux list-panes -t <session> -F "#{pane_id}:#{pane_index}:#{pane_active}:#{pane_width}:#{pane_height}"` |
| Capture scrollback | `tmux capture-pane -t <session> -p -S -<N>` |
| Send literal text | `tmux send-keys -t <session> -l <text>` |
| Send Enter | `tmux send-keys -t <session> Enter` |
| Attach to session | `tmux attach-session -t <name>` (executed in shell via `exec`) |
| Rename session | `tmux rename-session -t <old> <new>` |
| Split window | `tmux split-window -t <session>` |

### 3.3 TmuxSession / TmuxPane Data Structs

Identical to ai-commander's structs. `TmuxSession::parse` handles
`"name:timestamp:group"` format from `list-sessions -F`. `TmuxPane::parse`
handles `"id:index:active:width:height"` format.

### 3.4 TmuxError

```rust
#[derive(Error, Debug)]
pub enum TmuxError {
    #[error("tmux not found in PATH")]
    NotFound,
    #[error("session not found: {0}")]
    SessionNotFound(String),
    #[error("pane not found: {0} in session {1}")]
    PaneNotFound(String, String),
    #[error("tmux command failed: {0}")]
    CommandFailed(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
```

---

## 4. Adapter Interface

### 4.1 HarnessAdapter Trait

`src/adapters/traits.rs` — the core interface every harness plugin must implement.

```rust
/// State of a harness session as observed from pane output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HarnessState {
    Starting,
    Idle,
    Working,
    Paused,
    Error,
    Stopped,
}

/// Confidence-annotated detection result.
#[derive(Debug, Clone)]
pub struct DetectionResult {
    pub matched: bool,
    /// 0.0 – 1.0
    pub confidence: f32,
    /// Which pattern name fired.
    pub pattern: Option<&'static str>,
}

/// Observation of a session from captured pane output.
#[derive(Debug, Clone)]
pub struct HarnessObservation {
    pub state: HarnessState,
    pub confidence: f32,
    pub errors: Vec<String>,
}

/// Static metadata about a harness adapter.
#[derive(Debug, Clone)]
pub struct AdapterInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    /// Default launch command.
    pub command: &'static str,
    pub default_args: &'static [&'static str],
}

/// Core trait every harness adapter must implement.
pub trait HarnessAdapter: Send + Sync {
    fn info(&self) -> &AdapterInfo;

    /// Returns true (with confidence) if this adapter recognises pane output as
    /// belonging to its harness. Used for auto-detection.
    fn detect(&self, pane_output: &str) -> DetectionResult;

    /// Observe the current state of the session from pane output.
    fn observe(&self, pane_output: &str) -> HarnessObservation;

    /// Return the tmux send-keys payload to pause the session.
    /// None if the harness has no pause concept.
    fn pause_command(&self) -> Option<&'static str>;

    /// Return the tmux send-keys payload to resume the session.
    /// None if the harness has no resume concept.
    fn resume_command(&self) -> Option<&'static str>;

    /// Format a user message to send to the harness.
    fn format_message(&self, message: &str) -> String {
        message.to_string()
    }

    /// Returns patterns (regex strings) that identify idle state.
    fn idle_patterns(&self) -> &[&'static str];

    /// Returns patterns that identify error state.
    fn error_patterns(&self) -> &[&'static str];

    /// Returns patterns that identify working state.
    fn working_patterns(&self) -> &[&'static str];

    /// Patterns present in pane output that identify this harness brand.
    /// Used by auto-detection — higher specificity than idle/working patterns.
    fn brand_patterns(&self) -> &[&'static str];
}
```

### 4.2 Pattern Infrastructure

Reuse open-mpm's existing `regex` dependency. Port `Pattern` and helper functions
from `ai-commander/crates/commander-adapters/src/patterns.rs` into
`src/adapters/patterns.rs`:

```rust
pub struct Pattern {
    pub name: &'static str,
    regex: Regex,          // compiled once via OnceLock
    pub confidence: f32,
}

impl Pattern {
    pub fn matches(&self, text: &str) -> bool;
    pub fn captures(&self, text: &str) -> Option<Vec<String>>;
}

pub fn any_match(text: &str, patterns: &[Pattern]) -> bool;
pub fn best_match<'a>(text: &str, patterns: &'a [Pattern]) -> Option<&'a Pattern>;
```

---

## 5. Adapters to Implement

### 5.1 claude-mpm (`src/adapters/claude_mpm.rs`)

```rust
impl HarnessAdapter for ClaudeMpmAdapter {
    fn info(&self) -> &AdapterInfo { /* id: "claude-mpm", command: "claude-mpm" */ }

    fn detect(&self, output: &str) -> DetectionResult {
        // Brand patterns: "PM ready", "MPM", "delegate", "orchestrat"
    }

    fn pause_command(&self) -> Option<&'static str> {
        Some("/mpm-session-pause")
    }

    fn resume_command(&self) -> Option<&'static str> {
        Some("/mpm-session-resume")
    }

    fn idle_patterns(&self) -> &[&'static str] {
        &[r"(?i)PM ready", r"(?i)awaiting instructions", r"(?m)^>\s*$", r"\[IDLE\]"]
    }

    fn brand_patterns(&self) -> &[&'static str] {
        &[r"(?i)claude-mpm", r"(?i)MPM", r"(?i)orchestrat", r"(?i)delegat"]
    }
}
```

**Pause/resume mechanism**: claude-mpm REPL listens for `/mpm-session-pause` and
`/mpm-session-resume` slash commands. TM sends these via `tmux send-keys -l`.

### 5.2 claude-code (`src/adapters/claude_code.rs`)

```rust
impl HarnessAdapter for ClaudeCodeAdapter {
    fn info(&self) -> &AdapterInfo { /* id: "claude-code", command: "claude" */ }

    fn detect(&self, output: &str) -> DetectionResult {
        // Brand patterns: Claude prompt ">" with Unicode box chars, "claude" in pane title
    }

    fn pause_command(&self) -> Option<&'static str> {
        // Claude Code has no built-in pause; use Ctrl-C to interrupt
        None
    }

    fn resume_command(&self) -> Option<&'static str> {
        None
    }

    fn idle_patterns(&self) -> &[&'static str] {
        &[r"(?m)^>\s*$", r"(?i)waiting for input", r"\[IDLE\]"]
    }

    fn brand_patterns(&self) -> &[&'static str] {
        &[
            r"Claude Code",
            r"claude\.ai",
            r"dangerously-skip-permissions",
            r"✻",           // Claude Code working indicator
        ]
    }
}
```

**Pause/resume**: Claude Code has no session pause. TM can send Ctrl-C
(`tmux send-keys -t <session> C-c`) to interrupt, but there is no way to resume
without re-entering a prompt. TM should document this limitation and mark the
session as `Idle` after interrupt rather than `Paused`.

### 5.3 codex (`src/adapters/codex.rs`)

```rust
impl HarnessAdapter for CodexAdapter {
    fn info(&self) -> &AdapterInfo { /* id: "codex", command: "codex" */ }

    fn detect(&self, output: &str) -> DetectionResult {
        // Brand patterns: "Codex", "openai", codex prompt chars
    }

    fn pause_command(&self) -> Option<&'static str> { None }
    fn resume_command(&self) -> Option<&'static str> { None }

    fn idle_patterns(&self) -> &[&'static str] {
        &[r"(?m)^>\s*$", r"(?i)waiting for input", r"\[IDLE\]"]
    }

    fn brand_patterns(&self) -> &[&'static str] {
        &[r"(?i)\bcodex\b", r"openai", r"(?i)codex-cli"]
    }
}
```

### 5.4 augment-code (`src/adapters/augment.rs`)

```rust
impl HarnessAdapter for AugmentAdapter {
    fn info(&self) -> &AdapterInfo { /* id: "augment", command: "auggie" */ }

    fn pause_command(&self) -> Option<&'static str> { None }
    fn resume_command(&self) -> Option<&'static str> { None }

    fn brand_patterns(&self) -> &[&'static str] {
        &[r"(?i)augment", r"(?i)auggie", r"(?i)augment\.com"]
    }
}
```

### 5.5 gemini-code (`src/adapters/gemini.rs`)

```rust
impl HarnessAdapter for GeminiAdapter {
    fn info(&self) -> &AdapterInfo { /* id: "gemini", command: "gemini" */ }

    fn pause_command(&self) -> Option<&'static str> { None }
    fn resume_command(&self) -> Option<&'static str> { None }

    fn brand_patterns(&self) -> &[&'static str] {
        &[r"(?i)gemini", r"(?i)google.*ai", r"Gemini Code"]
    }
}
```

### 5.6 shell (`src/adapters/shell.rs`)

Used for plain terminal sessions with no AI harness. Idle patterns are shell prompts.

```rust
fn brand_patterns(&self) -> &[&'static str] {
    &[
        r"(?m)[$]\s*$",   // bash
        r"(?m)[%]\s*$",   // zsh
        r"(?m)[#]\s*$",   // root
        r"\w+@\w+",       // user@host
    ]
}
fn pause_command(&self) -> Option<&'static str> { None }
fn resume_command(&self) -> Option<&'static str> { None }
```

### 5.7 open-mpm (native)

Identifies the open-mpm PM REPL itself, as a self-aware adapter.

```rust
fn brand_patterns(&self) -> &[&'static str] {
    &[r"open-mpm", r"PM orchestrator", r"(?m)^>\s*$"]
}
fn pause_command(&self) -> Option<&'static str> { None }
fn resume_command(&self) -> Option<&'static str> { None }
```

---

## 6. Adapter Registry

`src/adapters/registry.rs` — centralized store and auto-detection.

```rust
pub struct AdapterRegistry {
    adapters: HashMap<&'static str, Arc<dyn HarnessAdapter>>,
}

impl AdapterRegistry {
    /// Instantiates all built-in adapters.
    pub fn new() -> Self;

    /// Return adapter by id.
    pub fn get(&self, id: &str) -> Option<Arc<dyn HarnessAdapter>>;

    /// List all adapter ids.
    pub fn list(&self) -> Vec<&'static str>;

    /// Auto-detect which adapter matches `pane_output` by running all brand
    /// pattern checks and returning the adapter with highest confidence.
    ///
    /// Returns (adapter, confidence). Falls back to ShellAdapter when no brand
    /// matches.
    pub fn detect(&self, pane_output: &str) -> (Arc<dyn HarnessAdapter>, f32);
}
```

---

## 7. Session Registry

`src/tm/registry.rs` — JSON-backed registry at
`.open-mpm/state/tm_sessions.json`.

### 7.1 On-Disk Format

```json
{
  "schema_version": 1,
  "sessions": [
    {
      "id": "a1b2c3d4-...",
      "name": "api-work",
      "project_id": "proj-uuid",
      "project_path": "/Users/masa/Projects/my-api",
      "adapter_type": "ClaudeCode",
      "tmux_session_name": "api-work",
      "status": "Running",
      "created_at": "2026-05-04T10:00:00Z",
      "last_active": "2026-05-04T11:23:00Z",
      "notes": null
    }
  ],
  "projects": [
    {
      "id": "proj-uuid",
      "name": "my-api",
      "path": "/Users/masa/Projects/my-api",
      "session_ids": ["a1b2c3d4-..."],
      "created_at": "2026-05-04T10:00:00Z"
    }
  ]
}
```

### 7.2 TmSessionRegistry API

```rust
pub struct TmSessionRegistry {
    path: PathBuf,
}

impl TmSessionRegistry {
    pub fn open(state_dir: &Path) -> Result<Self>;

    // Session CRUD
    pub fn register_session(&self, session: &TmSession) -> Result<()>;
    pub fn update_session_status(&self, id: &str, status: SessionStatus) -> Result<()>;
    pub fn touch_session(&self, id: &str) -> Result<()>;  // update last_active
    pub fn remove_session(&self, id: &str) -> Result<()>;
    pub fn list_sessions(&self) -> Result<Vec<TmSession>>;
    pub fn get_session(&self, id: &str) -> Result<Option<TmSession>>;
    pub fn get_session_by_name(&self, name: &str) -> Result<Option<TmSession>>;

    // Project CRUD
    pub fn register_project(&self, project: &TmProject) -> Result<()>;
    pub fn list_projects(&self) -> Result<Vec<TmProject>>;
    pub fn get_project(&self, id: &str) -> Result<Option<TmProject>>;

    // Lifecycle helpers
    pub fn reconcile(&self, live_sessions: &[String]) -> Result<Vec<String>>;
    // ^^ Marks sessions whose tmux names are not in live_sessions as Orphaned.
    // Returns list of orphaned session ids.
}
```

The registry reads the JSON file on every call (same pattern as `SessionsRegistry`)
so multiple processes share state via the file without a database.

---

## 8. Auto-Detection

`src/tm/manager.rs` contains `TmManager::detect_session_adapter`, which:

1. Calls `TmuxOrchestrator::capture_output(session_name, None, Some(100))` to get
   the last 100 lines of scrollback.
2. Passes the output to `AdapterRegistry::detect(pane_output)`.
3. `detect()` iterates all adapters, calls `adapter.detect(pane_output)`, and
   returns the adapter with the highest `confidence`.

### Detection Patterns Summary

| Harness | High-confidence brand patterns |
|---|---|
| claude-mpm | `"PM ready"`, `"claude-mpm"`, `"MPM"`, `"delegat"` |
| claude-code | `"Claude Code"`, `"✻"`, `"dangerously-skip-permissions"` |
| codex | `"\bcodex\b"` (case-insensitive), `"codex-cli"` |
| augment | `"augment"`, `"auggie"`, `"augment.com"` |
| gemini | `"Gemini Code"`, `"google.*ai"` |
| open-mpm | `"open-mpm"`, `"PM orchestrator"` |
| shell | fallback — any shell prompt pattern |

### Confidence Tiers

- **1.0**: Exact product name found in output
- **0.9**: Strong indicator (command flag or URL)
- **0.8**: Working indicator (typical prompt + context)
- **0.5**: Shell fallback (generic prompt only)

### Anti-ambiguity Rule

If two adapters both score ≥ 0.8, prefer the one whose brand patterns produce a
higher-confidence match. If still tied, prefer the first registered adapter. Log a
`warn!` so the user can investigate.

---

## 9. TmManager — High-Level Operations

`src/tm/manager.rs` provides the async business logic layer above the orchestrator
and registry.

```rust
pub struct TmManager {
    tmux: TmuxOrchestrator,
    adapters: Arc<AdapterRegistry>,
    registry: TmSessionRegistry,
}

impl TmManager {
    pub fn new(state_dir: &Path) -> Result<Self>;

    /// Create a new tmux session, register it, and optionally launch the harness.
    pub async fn new_session(
        &self,
        name: &str,
        project_path: &Path,
        adapter_type: Option<AdapterType>,
    ) -> Result<TmSession>;

    /// List all known sessions, reconciling with live tmux sessions first.
    pub async fn list_sessions(&self) -> Result<Vec<TmSession>>;

    /// Attach to a session (exec tmux attach in the calling terminal).
    pub async fn attach_session(&self, name_or_id: &str) -> Result<()>;

    /// Pause a session (send pause command via adapter).
    pub async fn pause_session(&self, name_or_id: &str) -> Result<()>;

    /// Resume a session (send resume command via adapter).
    pub async fn resume_session(&self, name_or_id: &str) -> Result<()>;

    /// Kill a tmux session and mark it Stopped in the registry.
    pub async fn kill_session(&self, name_or_id: &str) -> Result<()>;

    /// Send a message to the harness in the named session.
    pub async fn send_message(&self, name_or_id: &str, message: &str) -> Result<()>;

    /// Capture current pane output for inspection.
    pub async fn capture_pane(&self, name_or_id: &str, lines: u32) -> Result<String>;

    /// Auto-detect the adapter for a running session.
    pub async fn detect_adapter(&self, tmux_session_name: &str) -> Result<(AdapterType, f32)>;

    /// Reconcile registry with live tmux sessions, marking orphans.
    pub async fn reconcile(&self) -> Result<Vec<String>>;
}
```

---

## 10. REPL Integration

New slash commands registered in `src/repl/mod.rs`:

```
/tm new [name] [-p <path>] [-a <adapter>]   Create a new TM-managed tmux session
/tm list                                     List all TM sessions with status
/tm attach <name>                            Attach to a session (replaces current terminal)
/tm pause <name>                             Send pause command via adapter
/tm resume <name>                            Send resume command via adapter
/tm kill <name>                              Kill session and remove from registry
/tm send <name> <message>                    Send a message to the session's harness
/tm capture <name> [lines=50]               Capture and display pane output
/tm detect <name>                            Auto-detect and display adapter type
/tm reconcile                                Sync registry with live tmux sessions
/tm status [name]                            Show session detail or all session summary
```

### REPL Command Dispatch

Add to the `match cmd` block in `try_handle_slash`:

```rust
"/tm" => {
    if let Err(e) = self.handle_tm_command_into(arg, &mut out).await {
        let _ = writeln!(out, "tm error: {e:#}");
    }
    Ok(true)
}
```

`handle_tm_command_into` is implemented in `src/tm/commands.rs` as a free function
or `TmCommands` struct that takes `&mut TmManager` and dispatches on `arg`.

### /tm list output format

```
TM Sessions
──────────────────────────────────────────────
  NAME           ADAPTER       STATUS     LAST ACTIVE
  api-work       claude-code   Running    2m ago
  frontend       claude-mpm    Paused     1h ago
  scratch        shell         Idle       5m ago
──────────────────────────────────────────────
3 sessions  (2 running, 1 paused)
```

---

## 11. Module Structure — Files to Create

```
src/
├── tmux/
│   ├── mod.rs            pub use orchestrator::*, session::*, error::*
│   ├── orchestrator.rs   TmuxOrchestrator
│   ├── session.rs        TmuxSession, TmuxPane
│   └── error.rs          TmuxError
├── adapters/
│   ├── mod.rs            pub use traits::*, registry::*
│   ├── traits.rs         HarnessAdapter, HarnessState, DetectionResult, etc.
│   ├── patterns.rs       Pattern, any_match, best_match (ported from ai-commander)
│   ├── registry.rs       AdapterRegistry
│   ├── claude_mpm.rs
│   ├── claude_code.rs
│   ├── codex.rs
│   ├── augment.rs
│   ├── gemini.rs
│   ├── shell.rs
│   └── open_mpm.rs
└── tm/
    ├── mod.rs
    ├── project.rs        TmProject, TmSession, AdapterType, SessionStatus
    ├── registry.rs       TmSessionRegistry
    ├── manager.rs        TmManager
    └── commands.rs       handle_tm_command_into, /tm sub-commands
```

### Modifications to Existing Files

- `src/main.rs` — instantiate `TmManager` when `--tm` flag or TM feature enabled;
  pass to REPL.
- `src/repl/mod.rs` — add `tm_manager: Option<Arc<TmManager>>` field; add `/tm`
  match arm.
- `Cargo.toml` — no new dependencies required (uses existing `regex`, `serde`,
  `serde_json`, `chrono`, `anyhow`, `thiserror`, `tokio`).

---

## 12. Code Patterns from ai-commander to Port

| Pattern | Source location | Notes |
|---|---|---|
| `TmuxOrchestrator::run_tmux` + `run_tmux_checked` | `commander-tmux/src/orchestrator.rs:51-73` | Clean error mapping; port verbatim |
| `TmuxSession::parse` with `splitn(3, ':')` for group | `commander-tmux/src/session.rs:45-78` | Handles session groups correctly |
| `TmuxPane::parse` with 5-field format | `commander-tmux/src/session.rs:111-139` | Port verbatim |
| Dual `send-keys` pattern (literal then Enter) | `orchestrator.rs:327-343` | Critical: avoids "Enter" as literal text |
| `OnceLock<Vec<Pattern>>` for lazy static patterns | `commander-adapters/src/patterns.rs:54-91` | Avoids `lazy_static` dependency |
| `analyze_recent_output(lines: usize)` windowing | Every adapter `*_adapter.rs:28-60` | Only check last N lines for state |
| `best_match` + `any_match` helpers | `patterns.rs:209-218` | Composable; port as-is |
| `AdapterRegistry::detect` by highest confidence | (to design) | ai-commander doesn't have this yet; TM adds it |
| Session group deduplication in `list_sessions` | `orchestrator.rs:133-172` | Important for tmux environments with session groups |

### Key Implementation Note: Dual send-keys

From `orchestrator.rs` line 340-344:
```rust
// Send text literally (-l flag prevents interpreting as key names)
// Then send Enter separately to execute
self.run_tmux_checked(&["send-keys", "-t", &target, "-l", text])?;
self.run_tmux_checked(&["send-keys", "-t", &target, "Enter"])?;
```

This is non-obvious and critical. A single `send-keys -l "foo\nEnter"` does NOT
work. The two-call split is required.

---

## 13. GitHub Issues to Create

### Issue 1: Core tmux module — TmuxOrchestrator

**Title**: `feat: add src/tmux/ — TmuxOrchestrator wrapping tmux binary`

**Scope**:
- Create `src/tmux/error.rs`, `src/tmux/session.rs`, `src/tmux/orchestrator.rs`,
  `src/tmux/mod.rs`
- Port `TmuxOrchestrator` from ai-commander verbatim (already Rust, well-tested)
- Port `TmuxSession::parse`, `TmuxPane::parse`
- Add unit tests from ai-commander (they work as-is)
- Gate integration tests behind `#[cfg(tmux_integration)]`

**Acceptance**: `cargo test -p open-mpm -- tmux` passes; `TmuxOrchestrator::is_available()` works.

---

### Issue 2: Adapter trait + pattern infrastructure

**Title**: `feat: add src/adapters/ — HarnessAdapter trait and Pattern infrastructure`

**Scope**:
- `src/adapters/traits.rs`: `HarnessAdapter`, `HarnessState`, `DetectionResult`,
  `HarnessObservation`, `AdapterInfo`
- `src/adapters/patterns.rs`: `Pattern` (with `OnceLock`), `any_match`, `best_match`
- Unit tests for pattern matching

**Acceptance**: `HarnessAdapter` compiles as `dyn HarnessAdapter + Send + Sync`.

---

### Issue 3: Implement all harness adapters + registry

**Title**: `feat: implement claude-mpm, claude-code, codex, augment, gemini, shell adapters`

**Scope**:
- One file per adapter in `src/adapters/`
- `src/adapters/registry.rs` with `AdapterRegistry::detect()` auto-detection
- Brand patterns per section 5 above
- Pause/resume commands per section 5 above
- Unit tests for each adapter's `detect()` and `observe()`

**Acceptance**: All adapters register; `detect()` returns correct adapter for
sample pane output strings for each harness.

---

### Issue 4: TM data model + JSON registry

**Title**: `feat: add src/tm/project.rs + registry.rs — TmSession and TmSessionRegistry`

**Scope**:
- `TmProject`, `TmSession`, `AdapterType`, `SessionStatus` in `src/tm/project.rs`
- `TmSessionRegistry` backed by `.open-mpm/state/tm_sessions.json`
- Methods: `register_session`, `update_session_status`, `touch_session`,
  `remove_session`, `list_sessions`, `reconcile`
- Unit tests matching pattern from `session_registry.rs`

**Acceptance**: Round-trip serialization test passes; `reconcile` correctly marks
orphaned sessions.

---

### Issue 5: TmManager — high-level session operations

**Title**: `feat: add src/tm/manager.rs — TmManager combining orchestrator + adapters + registry`

**Scope**:
- `TmManager::new`, `new_session`, `list_sessions`, `attach_session`,
  `pause_session`, `resume_session`, `kill_session`, `send_message`,
  `capture_pane`, `detect_adapter`, `reconcile`
- `attach_session` must exec `tmux attach-session -t <name>` in the current
  terminal (use `std::process::Command::exec` on Unix or open a new pane)
- Integration with registry (auto-touch on send, auto-reconcile on list)

**Acceptance**: `cargo test -p open-mpm -- tm::manager` passes; manual test of
create/pause/resume/kill cycle with a running claude-mpm session.

---

### Issue 6: REPL /tm commands

**Title**: `feat: add /tm slash commands to open-mpm REPL`

**Scope**:
- `src/tm/commands.rs`: dispatcher for `/tm <sub>` commands
- Add `tm_manager: Option<Arc<TmManager>>` to `OpenMpmRepl`
- Add `/tm` match arm to `try_handle_slash`
- Sub-commands: `new`, `list`, `attach`, `pause`, `resume`, `kill`, `send`,
  `capture`, `detect`, `reconcile`, `status`
- Tab completion for session names after `/tm attach`, `/tm pause`, etc.

**Acceptance**: All `/tm` sub-commands documented in `/help`; `/tm list` renders
table; `/tm new foo -a claude-code` creates a real tmux session.

---

### Issue 7: Auto-detection on /tm list + reconcile

**Title**: `feat: auto-detect adapter type for unrecognised TM sessions`

**Scope**:
- When `list_sessions` finds a live tmux session not in the registry, run
  `detect_adapter` and register it automatically with `AdapterType::Unknown`
  upgraded to the detected type
- Expose via `/tm reconcile` — output shows sessions added, sessions orphaned
- Log detection confidence at `debug!` level

**Acceptance**: `/tm reconcile` picks up a running claude-mpm session and
correctly labels it; `AdapterType` is set to `ClaudeMpm`.

---

### Issue 8: Lifecycle state machine + idle monitoring

**Title**: `feat: background idle monitor for TM sessions`

**Scope**:
- `TmMonitor` (optional background tokio task) that every 30s:
  - Captures last 20 lines from each `Running` session
  - Calls `adapter.observe(output)`
  - Updates `status` to `Idle` or `Working` in registry
  - Logs `warn!` for any session in `Error` state
- Controlled by `TmManager::start_monitor()` / `stop_monitor()`
- Surface via `/tm status <name>` showing live state

**Acceptance**: After sending a long task to a claude-code session, `/tm status`
transitions from `Working` to `Idle` within 30s of task completion.

---

## 14. Storage Layout

```
.open-mpm/
└── state/
    ├── sessions.json          ← existing: PM session records
    └── tm_sessions.json       ← NEW: TM session + project records
```

The TM registry is separate from the existing `SessionsRegistry` to avoid coupling
the two concerns. Both live under `.open-mpm/state/` per the established convention.

---

## 15. Open Questions / Future Work

1. **Attach UX**: `tmux attach-session` replaces the calling terminal. For the
   open-mpm REPL (which runs in ratatui alt-screen), attaching is destructive.
   Option A: exec attach in a new tmux window. Option B: show a warning and require
   the user to run attach outside the REPL. Decision deferred to Issue 6.

2. **claude-code pause**: No native pause. Consider sending Ctrl-C to interrupt the
   current task. This maps to `SessionStatus::Idle`, not `Paused`. Adapters should
   expose `can_pause() -> bool`.

3. **gemini-code brand patterns**: Gemini Code CLI is less mature; patterns need
   verification against real output. Start conservative (exact string match).

4. **Multi-window sessions**: The TM design assumes one pane per session for
   simplicity. Extending to multi-window / multi-pane is post-MVP.

5. **Session naming collisions**: Two sessions named "api-work" in different
   projects need disambiguation. Suggested scheme: auto-suffix with `-2`, `-3` like
   tmux does for windows.

6. **REPL attach race**: When `attach_session` replaces the terminal, the REPL
   process is suspended. On detach, the REPL should re-render. This requires
   tracking the SIGCONT signal.

---

## 16. Summary of Key Design Decisions

| Decision | Choice | Rationale |
|---|---|---|
| Tmux wrapper | Synchronous `std::process::Command` | Calls are sub-ms; no async complexity needed |
| Registry storage | JSON flat file (not redb) | Human-readable, diffable, same pattern as sessions.json |
| Adapter detection | Pattern matching on captured pane output | No process introspection needed; works for remote sessions too |
| Pause mechanism | Send adapter-specific slash command | Adapter-defined; zero coupling to harness internals |
| Module layout | `src/tmux/`, `src/adapters/`, `src/tm/` | Clean separation of primitives / plugins / domain logic |
| New dependencies | None | regex, serde, chrono, anyhow already present |
| Code origin | Port from ai-commander commander-tmux crate | Tested, idiomatic Rust; avoids reinvention |
