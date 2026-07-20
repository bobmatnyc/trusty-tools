---
spec_refs:
  - id: SPEC-TCUI-01~draft
    path: docs/specs/trusty-code-harness-ui.md
    anchor: SPEC-TCUI-01~draft
  - id: SPEC-TCUI-09~draft
    path: docs/specs/trusty-code-harness-ui.md
    anchor: SPEC-TCUI-09~draft
  - id: SPEC-WS-08~draft
    path: docs/specs/DOC-48-tcode-workstreams.md
    anchor: SPEC-WS-08~draft
  - id: SPEC-SLD-02~draft
    path: docs/specs/spec-linked-documentation.md
    anchor: SPEC-SLD-02~draft
---

# DOC-50 — trusty-code Interactive TUI: Claude Code Clone over Shared REPL Layer

**Status:** Draft
**Subsystem:** trusty-code — interactive terminal UI (TUI) thin client; shared ratatui REPL layer (extraction from trusty-agents)
**Owner:** Engineering (trusty-code)
**Last-updated:** 2026-07-20
**Spec ID:** `SPEC-TTUI-01~draft` … `SPEC-TTUI-09~draft` (DOC-50)
**Builds on:**
- [`docs/specs/trusty-code-harness-ui.md`](./trusty-code-harness-ui.md) (DOC-39, merged) — [`SPEC-TCUI-01~draft`](./trusty-code-harness-ui.md#SPEC-TCUI-01~draft) §1 and [`SPEC-TCUI-09~draft`](./trusty-code-harness-ui.md#SPEC-TCUI-09~draft) §2.1 establish the **layer priority (API → CLI → TUI → Web)** and **thin-client axiom**: "The UI communicates with the daemon. All UI services talk to the daemon; the daemon provides all functionality." This spec is the TUI implementation of that constraint.
- [`docs/specs/DOC-48-tcode-workstreams.md`](./DOC-48-tcode-workstreams.md) (merged) — [`SPEC-WS-08~draft`](./DOC-48-tcode-workstreams.md#SPEC-WS-08~draft) §8 establishes workstream phasing. This spec's TUI must surface the active workstream (§4B below) and participate in workstream activation events (DOC-48 §5.3).
- [`crates/trusty-agents/src/repl/tui/`](../../../crates/trusty-agents/src/repl/tui/) (merged, second-gen tagent REPL) — The mature ratatui-based interactive REPL that is the **extraction target** and **reuse model** for this spec. Stack: ratatui 0.29 + crossterm 0.28, alt-screen + raw mode, mouse capture, 100ms-tick render loop (run.rs:197–251), mpsc `ReplEvent` channel architecture, slash-command framework, history recall + in-flight cancel, line editing (Ctrl-a/e/u/c/d), status line with daily-cost accumulator.
- [`docs/specs/spec-linked-documentation.md`](./spec-linked-documentation.md) (DOC-38, merged) — [`SPEC-SLD-02~draft`](./spec-linked-documentation.md#SPEC-SLD-02~draft) reference grammar and conventions.

**Cross-ref (merged code — prior art):**
- **Existing tagent REPL** (`crates/trusty-agents/src/repl/tui/`): the **reuse target**. Features: ratatui 0.29 widgets, slash-command routers (crate::repl::commands/*.rs, ~30 commands), model/agent pickers, LLM-offered choice lists, streaming input, Up-arrow history + Ctrl-c cancel, Ctrl-a/e/u/c/d line editing, mouse-wheel scroll (3-line notch), panic-safe terminal guard (crates/trusty-mpm/src/tui/coordinator/mod.rs:52–60 pattern).
- **Existing tm TUI crates** (`crates/trusty-mpm/src/tui/`): 3 TUIs under tmux-like conventions (coordinator dashboard, `tm sessions tui`, bare `tm projects` 4-pane). None share a widget library today; this extraction is the chance to establish one.
- **Existing tcode CLI client** (`crates/trusty-code/src/cli_client/{mod.rs,render.rs,stdio.rs}`): thin-client pattern that reads the `tcode serve --stdio` JSON-RPC API. The TUI extends this pattern to an interactive streaming loop, not a one-shot client.
- **Ratatui 0.30 migration path** (#2886, #2764): tagent REPL currently uses 0.29; 0.30 is available and removes a vulnerable transitive `lru` dep. The shared crate must decide on 0.30 adoption timing (§9, Q3 below).

> **Scope note.** This is a **functional spec**, not a design doc or implementation plan. It states what the product must do — the TUI's goal, architecture (shared crate + engine-adapter seam), extraction plan, and acceptance criteria — without prescribing exact Rust types or UI polish. The PR carrying this doc opens **no** Rust changes; it is a DRAFT spec for review. Per DOC-39 §2.1's binding layer-priority rule (API → CLI → TUI → Web), the thin-client contract (§2.1, C-1 through C-4) is the normative core; everything else is downstream of it.

---

## 1. Purpose and scope {#SPEC-TTUI-01~draft}

**ID:** SPEC-TTUI-01~draft
**Status:** Draft

### 1.1 The goal — Claude Code clone: interactive terminal UI over tcode daemon

**trusty-code has no TUI today, by design.** The one-shot `tcode run-task` CLI (in `crates/trusty-code/src/cli_client/`) is the current entry point. DOC-39 explicitly plans "a future TUI" and reserves the layer in its architecture; this spec is that implementation.

**The goal:** Build an **interactive terminal UI that mimics Claude Code's core UX** — streaming assistant output, tool-call rendering, an input composer, slash commands, scrollback, status line, and workstream awareness — while strictly adhering to DOC-39's **thin-client axiom** (§2.1): the TUI drives the existing `tcode serve --stdio` JSON-RPC API; all agent logic stays server-side in the daemon.

**What "Claude Code clone" means here:**
- **Streaming input/output:** Assistant responses stream to the TUI; the user sees partial output in real time.
- **Tool-call rendering:** When an agent invokes a tool (e.g., `fs.read`, `git.checkout`), the TUI renders the call and its result (as cards or inline, depending on phase).
- **Input composer:** A multi-line input prompt at the bottom, with history (Up-arrow), line editing, and in-flight cancel (Ctrl-C).
- **Slash commands:** `/help`, `/clear`, `/model`, `/agent`, `/workstream`, etc. — routed through the engine adapter to the daemon or handled client-side.
- **Scrollback:** Mouse-wheel scroll, page-up/down, history recall. Rendering is diff-based for performance.
- **Workstream awareness:** The TUI shows the active workstream name/ID and participates in activation events (DOC-48 §5.3).
- **Status line:** Session ID, current model, project name, workstream state, daily cost (loaded from the daemon), tmux session count (if observed).
- **Permission/diff prompts:** Future phases — the TUI surfaces permission gates and diffs for code changes (part of DOC-39 Phase 2).

**Non-requirement:** This spec does NOT aim for pixel-perfect parity with Claude Code's visual design. The goal is **functional parity** in interaction model and information architecture.

### 1.2 Non-goals

1. **Not a replacement for the web/Tauri GUI.** DOC-39 commits to an SPA (web + Tauri shell); this TUI is an *alternative* entry point, not the primary one. Both coexist.
2. **Not a general-purpose IDE.** The Project/IDE half (DOC-39 §7, Q4) remains out of scope.
3. **Not feature-complete on day 1.** MVP is the core streaming loop + essential tool rendering + basic slash commands (§4, MVP). Permission/diff prompts, workstream switcher UX, plain-line fallback (#3405) are later phases.
4. **Not SSH-hardened in MVP.** Issue #3405 (plain-line/no-TUI fallback for SSH/narrow terminals) is Phase 2.
5. **Not a fork of tagent's REPL.** The tagent REPL is the **extraction source**; tcode and tagent both consume the extracted shared crate (§3). Duplication is forbidden.

---

## 2. Thin-client architecture and engine-adapter seam {#SPEC-TTUI-02~draft}

**ID:** SPEC-TTUI-02~draft
**Status:** Draft

### 2.1 The thin-client axiom (DOC-39 §2.1, binding constraint) {#SPEC-TTUI-09~draft}

**ID:** SPEC-TTUI-09~draft
**Status:** Draft

The TUI is a **thin client**. This is not a recommendation; it is a binding architectural constraint from DOC-39:

**C-1 — The UI is a thin client.** No business logic, no local capability, no direct filesystem, process, or git access. The TUI renders daemon state and issues daemon calls. That is its entire job.

**C-2 — The daemon is the single source of functionality.** Anything the TUI needs MUST exist as a daemon API — a JSON-RPC method, an event, or both. **If a feature needs something the daemon cannot answer, that is an API gap to specify — never a UI-side workaround.**

**C-3 — No capability divergence between targets.** This rule applies equally to TUI, web, and Tauri. A feature MUST NOT exist in one UI target and not another. The TUI is a peer UI, not a special case.

**C-4 — Corollary: the TUI MUST NOT use ncurses, crossterm, or ratatui features to reach around the daemon.** It is not a shortcut — it violates C-1. Directly reading the filesystem, calling `git`, or spawning a shell is a violation. Every such fact arrives from a daemon call.

**AC-19.1** No TUI code reads the filesystem, spawns a process, or shells out to `git` directly. Every such fact arrives from a daemon call or event.

**AC-19.2** A capability matrix of "does tcode-tui have this, does web have this" is **empty by construction** — any row in it is a spec violation.

**AC-19.3** Client-side derivation of a value the daemon owns (e.g., local git-dirty detection, file-size computation) is a violation.

### 2.2 Architecture: shared TUI crate + engine adapter

**The crate hierarchy:**

```
trusty-tui (NEW shared crate, or trusty-common::tui feature)
├── Public exports: TuiEngine (trait), ReplUi (struct), ReplEvent, ReplConfig
├── Modules: widgets, layout, input, event_loop, pickers, status
└── No public exports of ratatui or crossterm (encapsulated)

trusty-code/src/tui_client (NEW, ~400–600 lines)
├── CodeEngine: impl TuiEngine  ← engine adapter (thin) 
└── Drives trusty-tui's ReplUi with daemon JSON-RPC calls

trusty-agents/src/repl/tui (REFACTORED, existing tagent REPL)
├── AgentEngine: impl TuiEngine  ← engine adapter (thin)
└── Uses trusty-tui's ReplUi and slash-command framework
```

**The engine-adapter trait:**

A single trait (`TuiEngine`) abstracts the **semantic difference** between tagent and tcode:

```rust
// In trusty-tui (shared crate)
#[async_trait::async_trait]
pub trait TuiEngine: Send + Sync {
    /// Process a submitted line (user input or slash command).
    /// Returns `Ok(true)` to keep looping, `Ok(false)` to quit.
    /// Pushes output through `tx` as ReplEvent variants.
    async fn handle_input(&self, line: String, tx: UnboundedSender<ReplEvent>) -> Result<bool>;
    
    /// Async setup: load initial state (model, agent roster, workstream, etc.).
    /// Called before the render loop starts.
    async fn setup(&self, tx: UnboundedSender<ReplEvent>) -> Result<()>;
    
    /// Graceful shutdown (optional). Close any open daemon connections.
    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }
}
```

**Key design:**
- **The TUI is engine-agnostic.** It does not know about `trusty-code` or `trusty-agents`. It only knows about `TuiEngine`, `ReplEvent`, and lifecycle.
- **The engine adapter is thin.** All it does is **translate user input into daemon calls** (JSON-RPC or otherwise) and **translate daemon responses into `ReplEvent` variants**. No rendering, no state machine beyond the request/response pattern.
- **Slash commands are routed by the TUI to the engine.** When the user types `/model`, the TUI parses it as a slash command and calls `engine.handle_input(…)`. The engine then calls the daemon's `model.list()`, renders via `tx.send(ReplEvent::…)`, and returns.

**Dependency direction (critical):**
```
trusty-code → trusty-tui
trusty-agents → trusty-tui
trusty-tui → (ratatui, crossterm, tokio, serde only)
```

trusty-tui does **not** depend on `trusty-code` or `trusty-agents`. Both products depend on `trusty-tui`.

### 2.3 Extraction plan: what moves to trusty-tui, what stays in tagent

**MOVE to trusty-tui:**
1. **Core event loop** (`crates/trusty-agents/src/repl/tui/run.rs:event_loop` and friends) — the 100ms-tick render loop, key reader thread, terminal setup/restore.
2. **ReplApp state machine** (chat scrollback, input buffer, history, line-edit cursor position, tick counters).
3. **Widgets** (scrollback area, status line, input composer, slash-command hint panel).
4. **Layout** (main window split: chat above, input below, status at bottom).
5. **Ratatui primitives** (Paragraph, Block, Constraints, Direction, Layout, draw fn).
6. **Panic-safe terminal guard** (`setup_terminal`, `restore_terminal` RAII pattern from tm's coordinator TUI).
7. **ReplEvent channel** (key press, resize, scroll, status message, assistant output, tool invocation, etc.).

**STAY in trusty-agents:**
1. **AgentEngine impl** — the agent-specific logic (agent roster, model picker, `/agent` command, persona switching).
2. **Slash-command handlers** (`repl/commands/*.rs`) — Agent-specific commands like `/model`, `/agent`, `/update`, `/code-review`, etc. (but the framework + dispatch logic moves to trusty-tui as `SlashCommandRouter`).
3. **Streaming orchestration** — How tagent handles streaming input from the daemon, merges concurrent agent events, etc.

**MOVE (extracted and shared):**
1. **Slash-command framework** (`SlashCommandRouter` trait + macro) — allows both engines to define commands. Generic name (e.g., `/help`, `/clear`), generic dispatch.
2. **Pickers** (model, agent, workstream) — UI framework; the data source comes from the engine (`/model` command calls the engine, which fetches from the daemon).
3. **Line editing** (Ctrl-a, Ctrl-e, Ctrl-u, Ctrl-c, Ctrl-d, backspace, Up-arrow history).

### 2.4 Design for resilience and testability

**Connection loss:** When the daemon connection is lost (e.g., daemon restart, network issue):
- The TUI remains live. The input loop continues; new input attempts to reconnect.
- A status message appears: "Connection lost — attempting reconnect…"
- Once reconnected, the TUI resumes normal operation (the daemon may have lost state, so the TUI may need to refetch the active workstream, etc.).

**Panic safety:** The `setup_terminal`/`restore_terminal` RAII guard ensures that even if the TUI panics, the terminal is restored to cooked mode (not corrupted with alt-screen left on).

**Testability:** The `TuiEngine` trait allows tests to mock the daemon. A test engine can return fixed responses without spawning a real daemon.

---

## 3. Extraction and migration plan {#SPEC-TTUI-03~draft}

**ID:** SPEC-TTUI-03~draft
**Status:** Draft

### 3.1 Step 1: Create the shared trusty-tui crate

**New crate:** `crates/trusty-tui/` (or add a `tui` feature to `trusty-common`; see Q2 below).

**Module layout:**

```
crates/trusty-tui/
├── Cargo.toml (ratatui 0.30*, crossterm, tokio, async-trait, serde, uuid)
├── src/
│   ├── lib.rs (public exports)
│   ├── engine.rs (TuiEngine trait definition)
│   ├── event.rs (ReplEvent enum, key codes, serialization)
│   ├── app.rs (ReplApp state struct — chat buffer, input, history, ticks)
│   ├── terminal.rs (setup_terminal, restore_terminal RAII; crossterm wrappers)
│   ├── input.rs (line editing: cursor position, word wrap, backspace, Ctrl-a/e/u/c/d, Up-arrow)
│   ├── event_loop.rs (run_tui async fn, tokio::select! pattern, 100ms tick)
│   ├── draw.rs (render fn; ratatui widget composition)
│   ├── widgets/
│   │   ├── mod.rs
│   │   ├── scrollback.rs (Paragraph + wrapping; diff-based partial updates)
│   │   ├── input_composer.rs (Paragraph + cursor; editing state)
│   │   ├── status_line.rs (Gauge, Span, session/model/workstream/cost)
│   │   ├── tool_card.rs (Block + Text for tool calls/results; Phase 2+)
│   │   └── picker.rs (Modal or inline list; model/agent/workstream pick UI)
│   ├── layout.rs (Constraints, Direction, Layout; 2 or 3-pane split)
│   ├── slash_commands.rs (SlashCommandRouter trait; dispatch table)
│   ├── statusline.rs (StatuslineConfig; time/date, session count, cost accumulation)
│   └── colors.rs (palette tokens; theme-aware via theme-aware terminal)
```

**Cargo.toml dependencies:**
```toml
ratatui = "0.30"    # Latest stable, non-vulnerable
crossterm = "0.28"
tokio = { version = "1", features = ["full"] }
async-trait = "0.1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
uuid = { version = "1", features = ["v4", "serde"] }
anyhow = "1"
thiserror = "1"
tracing = "0.1"
```

**Exports from lib.rs:**
```rust
pub use engine::TuiEngine;
pub use event::{ReplEvent, ReplEventKind};
pub use app::ReplApp;
pub use {run_tui, ReplStartup};
// NOT exported: ratatui, crossterm (encapsulated)
```

### 3.2 Step 2: Extract tagent REPL into trusty-tui

**PR 1 (extraction):** Move the above modules from `crates/trusty-agents/src/repl/tui/` to `trusty-tui/src/`.

**Compatibility layer (in tagent, Phase 1A):** Add a re-export:
```rust
// crates/trusty-agents/src/repl/tui/mod.rs (AFTER extraction)
pub use trusty_tui::{run_tui, ReplApp, ReplStartup, ReplEvent};
// Old imports from tagent still work for a transition period
```

**Tagent REPL behavior is unchanged.** No UX regression. The event loop and terminal behavior remain identical.

### 3.3 Step 3: Implement CodeEngine (tcode engine adapter)

**New file:** `crates/trusty-code/src/tui_client/engine.rs`

```rust
pub struct CodeEngine {
    project_path: PathBuf,
    daemon_url: String,  // e.g., "http://localhost:7882"
    http_client: HttpClient,
}

#[async_trait::async_trait]
impl TuiEngine for CodeEngine {
    async fn handle_input(&self, line: String, tx: UnboundedSender<ReplEvent>) -> Result<bool> {
        // Parse line: is it a slash command or a chat prompt?
        if line.starts_with('/') {
            self.handle_slash_command(&line, tx).await
        } else {
            // Send to daemon: task.run {prompt: line}
            // Stream responses as ReplEvent::AssistantOutput, ReplEvent::ToolInvocation, etc.
            self.handle_chat(&line, tx).await
        }
    }
    
    async fn setup(&self, tx: UnboundedSender<ReplEvent>) -> Result<()> {
        // Fetch initial state: active workstream, session ID, model, etc.
        // Call daemon APIs to populate initial state
        let ws = self.daemon_call::<Workstream>("workstream.get", ???).await?;
        tx.send(ReplEvent::WorkstreamUpdated(ws))?;
        Ok(())
    }
}
```

### 3.4 Step 4: Refactor tagent REPL to use AgentEngine adapter

**Refactor:** Move agent-specific logic in `TrustyAgentsRepl` into an `AgentEngine` impl.

**No behavioral change.** Tagent's REPL continues to work exactly as today.

### 3.5 Adoption order (critical for safety)

1. **Phase 1A.1:** Create `trusty-tui` crate (new, empty shell).
2. **Phase 1A.2:** Move only the **framework** (terminal setup, event loop, TuiEngine trait definition) to `trusty-tui`. No ratatui widgets yet; focus on the trait and event plumbing.
3. **Phase 1A.3:** Implement `CodeEngine` (thin) against the trait. Test that it compiles and runs (bare REPL, no output yet).
4. **Phase 1A.4:** Move the **widgets and rendering** to `trusty-tui`. Regression-test tagent's REPL (must work identically).
5. **Phase 1A.5:** Implement `AgentEngine` (thin) and refactor tagent to use it.
6. **Phase 1B+:** Add tcode-specific features (workstream switcher, toolcard rendering, etc.).

**Critical invariant:** At no point is tagent's REPL broken or regressed. Each PR is independently shippable with a working, tested REPL.

---

## 4. Phasing and MVP scope {#SPEC-TTUI-04~draft}

**ID:** SPEC-TTUI-04~draft
**Status:** Draft

### 4.1 MVP (Phase 1A) — Core streaming loop + essential slash commands

**Goal:** A working, shippable tcode TUI that does the core loop: user types a prompt, the daemon runs an agent, the TUI streams the output.

**Features:**
- Streaming chat input/output (assistant responses render in real time).
- Input composer (multi-line, history, line editing).
- Slash commands: `/clear` (clear scrollback), `/help` (list commands), `/quit` or Ctrl-D (exit), `/workstream list` (list workstreams), `/workstream activate <id>` (switch workstream).
- Status line: current session ID, active workstream, model, project name.
- Scrollback: Up-arrow history, mouse-wheel scroll, Page-up/down, Ctrl-u (clear input), Ctrl-c (cancel in-flight input).
- Tool invocations: render as text (not fancy cards yet) — e.g., `[TOOL] git.checkout: main` then `[RESULT] …`.
- Connection loss resilience: "Connection lost" status, auto-reconnect on next input.
- Workstream awareness: TUI shows active workstream and responds to `WorkstreamActivationChanged` events (DOC-48 §5.3).

**OUT of scope (Phase 2+):**
- Permission/diff prompts (await daemon RPC support).
- Fancy tool-call cards (Phase 2).
- Workstream switcher UI (Phase 2).
- Plain-line fallback for SSH (#3405, Phase 2).
- Model/agent pickers in the TUI (Phase 2; for now, use `/model <name>`, `/agent <name>` text commands).
- Rich suggestion list (Phase 2).

**Acceptance criteria (MVP):**
- `tcode tui` command exists and launches the REPL.
- User types a prompt, presses Enter, the daemon's response streams to the TUI.
- `/help` shows a list of slash commands.
- Scrollback works (Up-arrow, mouse-wheel, Page-up/down).
- Workstream switching works (user runs `/workstream activate <id>`, TUI switches and shows the new active workstream).
- Input composer history works (Up-arrow on empty input line recalls prior prompts).
- Ctrl-c cancels an in-flight response.
- On daemon disconnect, TUI survives and shows "Connection lost" status.

### 4.2 Phase 2 — Permission prompts, tool cards, UX refinement

**Features:**
- Permission/diff prompts: when the daemon requests approval (e.g., "Run this code? y/n"), the TUI surfaces a prompt and relays the response.
- Tool-call cards: fancy rendering of tool invocations (blocks, colors, indentation).
- Streaming diffs: code changes render incrementally (Phase 2B+).
- Model/agent pickers: modal/inline UI (reuse from tagent's pickers).
- SSH fallback (#3405): detect narrow terminals (< 80 cols?) and fall back to plain-line mode (no alt-screen, no ratatui).
- Workstream switcher UI: header showing active workstream name; Ctrl-w or `/ws activate` with inline picker.

**Acceptance criteria:**
- Permission prompts are surfaced and responses are relayed to the daemon.
- Tool calls render with colors and indentation.
- Users can pick models and agents via TUI modal (not just text commands).
- SSH mode works (plain-line fallback, no alt-screen).

### 4.3 Phase 3 (future) — Suggested next prompts, advanced workstream features

**Features:**
- Suggested next prompts (DOC-39, §7, Q: "Claude-Code-style suggested next-prompts" #2078).
- Workstream creation UI (not just `task.run` binding).
- Archive/history of prior workstreams and sessions.
- Inline file browser (context: #3405, project picker modal from DOC-48 Phase C+).

---

## 5. Implementation slices {#SPEC-TTUI-05~draft}

**ID:** SPEC-TTUI-05~draft
**Status:** Draft

Each slice is independently shippable and critic-gateable.

### Slice 1: trusty-tui framework scaffold (Phase 1A.1–1A.2)

**What:** Create the `trusty-tui` crate with the `TuiEngine` trait, `ReplEvent` enum, and event loop scaffold (no rendering yet).

**Deliverable:**
- `crates/trusty-tui/Cargo.toml` with ratatui 0.30, crossterm, tokio.
- `trusty_tui::TuiEngine` trait: `handle_input`, `setup`, `shutdown`.
- `trusty_tui::ReplEvent` enum: Key, Resize, Scroll, AssistantOutput, ToolInvocation, StatusMessage, WorkstreamUpdated, WorkstreamActivationChanged.
- `run_tui<H: TuiEngine>` async fn signature (not fully implemented yet, stub ok).
- Compile check only; no integration test yet.

**Acceptance:**
- Crate compiles with no warnings.
- `TuiEngine` trait is public and documented.
- `ReplEvent` enum covers all expected event types.

**Effort:** 1 engineer-day.

### Slice 2: Terminal setup and event loop (Phase 1A.2)

**What:** Implement the panic-safe terminal RAII guards, event channel, key reader thread, and the 100ms-tick loop structure.

**Deliverable:**
- `setup_terminal()` → Terminal with alt-screen + raw mode + mouse capture.
- `restore_terminal()` RAII guard (panic-safe).
- Key reader thread spawned; KeyEvent routed to mpsc channel.
- 100ms-tick interval.
- `event_loop<H: TuiEngine>` scaffold: `tokio::select!` pattern, tick + rx.recv() branches, process ReplEvent, call `engine.handle_input()`, loop on `app.quit`.

**Acceptance:**
- `cargo run -p trusty-tui --example minimal` enters alt-screen without panic on Ctrl-C.
- Key presses are captured and routed to the event channel.
- Tick fires every 100ms (counter increments).

**Effort:** 1 engineer-day.

### Slice 3: CodeEngine implementation (Phase 1A.2–1A.3)

**What:** Implement `CodeEngine` (the tcode engine adapter) that speaks to `tcode serve --stdio`.

**Deliverable:**
- `crates/trusty-code/src/tui_client/engine.rs`: `CodeEngine` struct, `TuiEngine` impl.
- `handle_input`: parse slash commands vs. chat, call daemon JSON-RPC (or stdio), stream responses as `ReplEvent::AssistantOutput`.
- `setup`: fetch initial workstream, session ID, etc.
- Bare-bones implementation: no error recovery yet.

**Acceptance:**
- Code compiles.
- Integration test: spawn a mock daemon, send a prompt via CodeEngine, verify ReplEvent is emitted.

**Effort:** 1 engineer-day.

### Slice 4: ReplApp state and basic widgets (Phase 1A.4)

**What:** Move `ReplApp` from tagent, implement scrollback (Paragraph widget), input composer (editable text), status line.

**Deliverable:**
- `trusty_tui::ReplApp` struct: chat messages, input buffer, history, cursor position, tick counter.
- `ReplApp::push_message(role: &str, text: String)` and `ReplApp::push_status(text: String)`.
- Widgets: scrollback (Paragraph with line wrapping), input composer (Block + editable area), status line (Gauge + Spans).
- `draw(Frame, ReplApp)` function: renders widgets to the frame.

**Acceptance:**
- `cargo test` passes for ReplApp state transitions.
- Visual test: spawn tcode tui, see scrollback + input + status line rendered correctly (no assertion, manual inspection).

**Effort:** 1.5 engineer-days.

### Slice 5: Event dispatch and line editing (Phase 1A.4)

**What:** Wire up key events (KeyCode::Enter, KeyCode::Backspace, KeyCode::Up, Ctrl-a/e/u/c/d) to app state transitions.

**Deliverable:**
- `process_event(event: ReplEvent, app: &mut ReplApp, handler: &TuiEngine)` function.
- KeyCode::Enter → call `handler.handle_input(input_buffer, tx)`, clear input.
- KeyCode::Backspace → remove char from input at cursor, move cursor back.
- KeyCode::Up → recall prior prompt from history.
- Ctrl-a → cursor to start of line.
- Ctrl-e → cursor to end of line.
- Ctrl-u → clear input.
- Ctrl-c → cancel in-flight request (set a flag; handler sees it on next poll).
- Ctrl-d → quit.

**Acceptance:**
- Unit test: push 5 key events in sequence, verify ReplApp state changes correctly.
- Visual test: type a line, press Up-arrow, verify prior prompt recalled; press Ctrl-a, verify cursor at start; backspace works.

**Effort:** 1 engineer-day.

### Slice 6: Workstream awareness and status line updates (Phase 1A.5)

**What:** Wire up workstream events; update status line to show active workstream name/ID.

**Deliverable:**
- `handle_event(ReplEvent::WorkstreamUpdated(ws))` updates `app.active_workstream`.
- `handle_event(ReplEvent::WorkstreamActivationChanged{new_id, prior_id})` triggers a daemon call to fetch the new workstream, updates display.
- Status line widget includes active workstream: "WS: Token rotation (a1b2c3d4)".
- On `/workstream activate <id>`, CodeEngine calls `workstream.activate{id}`, daemon responds with `WorkstreamActivationChanged` event, TUI updates display.

**Acceptance:**
- Unit test: emit WorkstreamUpdated(ws), verify app.active_workstream is updated.
- Integration test: call `workstream.activate`, verify TUI receives WorkstreamActivationChanged and updates status line.
- Visual test: run `tcode tui`, see active workstream in status line; run `/workstream activate <id>`, see status line update.

**Effort:** 1 engineer-day.

### Slice 7: Slash commands framework (Phase 1A.5)

**What:** Implement the slash-command router and handlers for `/clear`, `/help`, `/quit`, `/workstream list/activate`.

**Deliverable:**
- `SlashCommandRouter` trait: `dispatch(cmd: &str, args: &[&str], tx: UnboundedSender<ReplEvent>) -> Result<bool>`.
- Built-in commands (in trusty-tui): `/clear` → `ReplEvent::ClearScrollback`, `/help` → list all commands, `/quit` → `app.quit = true`.
- CodeEngine-specific commands: `/workstream list` → call daemon, emit `ReplEvent::StatusMessage(formatted list)`.
- CodeEngine-specific commands: `/workstream activate <id>` → call `workstream.activate{id}`, daemon responds.

**Acceptance:**
- Unit test: invoke `/clear`, verify ClearScrollback event is emitted.
- Integration test: invoke `/workstream list`, verify daemon is called and result is formatted as a status message.
- Visual test: type `/help`, press Enter, see command list; type `/clear`, see scrollback cleared.

**Effort:** 1.5 engineer-days.

### Slice 8: Tool invocation rendering (Phase 2)

**What:** When the daemon emits a tool invocation event, render it in the TUI (fancy card or inline text).

**Deliverable:**
- `ReplEvent::ToolInvocation{tool_name, args, result}`.
- Widget: render tool card (or inline text for MVP: `[TOOL] tool_name: args`, `[RESULT] result`).
- CodeEngine: on tool invocation from daemon, emit ReplEvent::ToolInvocation, TUI renders it.

**Acceptance:**
- Unit test: create a ToolInvocation event, verify it's rendered correctly.
- Integration test: simulate a tool call from the daemon, verify TUI displays it.
- Visual test: trigger a tool call (e.g., run a task that invokes git.checkout), see tool card in scrollback.

**Effort:** 1 engineer-day.

### Slice 9: Permission/diff prompts (Phase 2)

**What:** When the daemon requests approval (for code changes, file writes, etc.), the TUI surfaces a prompt and relays the response.

**Deliverable:**
- `ReplEvent::PermissionPrompt{action, allow_handler}` or similar (exact shape TBD with daemon API).
- UI modal or inline prompt: "Allow [action]? (y/n)".
- Key handler: Ctrl-y/Enter → send approval to daemon, Ctrl-n → reject.
- CodeEngine: on approval prompt from daemon, emit PermissionPrompt event, wait for user response, call daemon API to relay.

**Acceptance:**
- Unit test: emit PermissionPrompt, simulate user pressing 'y', verify daemon is called with approval.
- Integration test: trigger a permission prompt from the daemon, verify TUI surfaces it and relays response.
- Visual test: trigger a permission prompt, see modal, press y/n, see approval relayed.

**Effort:** 1.5 engineer-days.

### Slice 10: Tagent REPL refactoring (Phase 1A.5, critical for safety)

**What:** Refactor tagent's REPL to use the extracted `trusty-tui` crate and AgentEngine adapter.

**Deliverable:**
- Implement `AgentEngine` (thin adapter for tagent).
- Replace tagent's direct TUI calls with `run_tui<AgentEngine>`.
- Regression test: tagent's REPL behaves identically to before (no UX change).

**Acceptance:**
- Tagent's REPL runs without warnings.
- All existing slash commands work (e.g., `/model`, `/agent`, `/update`).
- Visual test: `tagent` launches REPL, user can type prompts and see responses, everything works as before.

**Effort:** 2 engineer-days (refactoring is tricky; requires care to avoid regressions).

---

## 6. Risks and open questions for Bob {#SPEC-TTUI-06~draft}

**ID:** SPEC-TTUI-06~draft
**Status:** Draft

### Q1 — Shared crate naming: trusty-tui vs trusty-common::tui feature?

**Context:** The extracted TUI layer could be a standalone crate (`crates/trusty-tui/`) or a feature-gated module in `trusty-common` (e.g., `crates/trusty-common/src/tui/`).

**Pros (standalone crate):**
- Clear isolation; no TUI deps leaking into trusty-common.
- Independent versioning and release.
- Larger consumers (future harnesses) can depend on it cleanly.

**Pros (trusty-common feature):**
- Fewer top-level crates; `trusty-common` is already a hub.
- Simpler CI config (one fewer crate to test).
- Mirrors other optional features (search, memory).

**Decision needed:** What is your preference?

**Recommendation:** Standalone `crates/trusty-tui/`. The TUI has ratatui + crossterm as hard deps, which are heavy and unrelated to trusty-common's core (types, logging, monitoring). Isolation is cleaner.

### Q2 — Ratatui 0.30 adoption timing?

**Context:** Tagent REPL currently uses ratatui 0.29. Ratatui 0.30 is available and removes a vulnerable transitive `lru` dep (#2886). The shared crate must pick a version.

**Options:**
1. Start with 0.29 (matches tagent today), upgrade later. Risk: delay fixing the vulnerability.
2. Start with 0.30 (latest stable), bump tagent during Slice 10. Risk: if 0.30 has incompatibilities, tagent refactoring becomes harder.
3. Decouple: shared crate uses 0.30, tagent stays on 0.29 (with a shim adapter). Risk: maintenance burden; two versions of similar code.

**Decision needed:** Which path?

**Recommendation:** Start with 0.30. It is stable and the vulnerability fix is important. Tagent's bump during Slice 10 is a clean refactoring; any incompatibilities will be caught by testing.

### Q3 — SSH/narrow-terminal fallback in MVP or Phase 2?

**Context:** Issue #3405 asks for a plain-line mode (no alt-screen, no ratatui) for SSH and narrow terminals. The TUI currently assumes a full-size terminal with alt-screen support.

**Options:**
1. MVP includes plain-line fallback: detect narrow term, fall back gracefully. Risk: more complex MVP; both code paths must work.
2. MVP ignores SSH (known issue); Phase 2 adds fallback. Risk: TUI is not usable over SSH until Phase 2.
3. Deferred: out of scope for this spec (leave for future TUI work).

**Decision needed:** When?

**Recommendation:** Phase 2. MVP focus is the core streaming loop. Plain-line fallback is a nice-to-have that can wait. Document the known limitation: "tcode tui requires a full-size terminal with alt-screen support; SSH/narrow terminals fall back to `tcode run-task` CLI."

### Q4 — Engine adapter flexibility: abstract task.run or allow engine to customize slash commands?

**Context:** The engine adapter is thin by design. But there's a question of **how thin**. Should every slash command be routable through the engine, or should some be handled client-side (e.g., `/help` is always the same, no engine call needed)?

**Options:**
1. All slash commands are routed to the engine: `engine.handle_input("/help")`. The engine decides what to do (emit help text or call the daemon).
2. Built-in commands (`/help`, `/clear`, `/quit`) are client-side; domain-specific commands (`/model`, `/agent`, `/workstream`) are routed to the engine.
3. Engines can define custom slash commands (e.g., AgentEngine defines `/update`, CodeEngine defines `/workstream`). The TUI's dispatch table is engine-populated.

**Decision needed:** How much flexibility do we need?

**Recommendation:** Option 2 (built-in commands are client-side, domain-specific are routed to the engine). It keeps the MVP simple and the engine lightweight. If engines need custom commands later, we can add Option 3.

### Q5 — Workstream activation in MVP: explicit `/workstream activate` or implicit switcher?

**Context:** MVP includes slash commands (e.g., `/workstream list`, `/workstream activate <id>`). Later phases add a visual switcher (e.g., header with a dropdown). Should both coexist, or is one enough for MVP?

**Decision needed:** For MVP, is text-command activation sufficient?

**Recommendation:** Yes. Text command is sufficient for MVP. The visual switcher is a Phase 2 UX refinement. This keeps MVP scope tight and shippable.

### Q6 — Model/agent pickers: inline list or modal?

**Context:** Phase 2 adds model/agent pickers (when the user runs `/model` or `/agent`). Should the picker be a modal (full-screen) or inline (a pop-up list below the input)?

**Pros (modal):** Matches Claude Code UX; clear focus; avoids scrollback interference.
**Pros (inline):** Less disruptive; faster context-switch; matches tagent's current picker style.

**Decision needed:** Which UX do you prefer for tcode?

**Recommendation:** Inline list (below input). Tagent's picker is familiar, and it's simpler to implement. If the visual design later calls for a modal, we can change it.

---

## 7. Acceptance Criteria {#SPEC-TTUI-07~draft}

**ID:** SPEC-TTUI-07~draft
**Status:** Draft

### AC-1: Thin-client contract (binding from DOC-39)

**AC-1.1** No TUI code (files under `crates/trusty-tui/` or `crates/trusty-code/src/tui_client/`) reads the filesystem, spawns a process, or calls `git` directly. Every such fact arrives from a daemon call.

**AC-1.2** The TUI has no dependency on `trusty-code` or `trusty-agents` in its public API. Only `TuiEngine`, `ReplEvent`, and lifecycle items are public.

**AC-1.3** The `TuiEngine` trait is implemented by each product crate independently. No shared implementation.

### AC-2: Extraction success (trusty-tui shared crate)

**AC-2.1** `trusty-tui` crate is created and compiles with ratatui 0.30.

**AC-2.2** `TuiEngine` trait is defined and documented. Both `CodeEngine` and `AgentEngine` implement it without breaking changes.

**AC-2.3** Tagent's REPL is refactored to use `trusty-tui` with **zero UX regression**. All existing commands work; scrollback, history, line editing are identical.

**AC-2.4** CodeEngine is implemented and tcode can launch a REPL with `tcode tui` command.

### AC-3: MVP feature parity

**AC-3.1** Streaming chat: user types a prompt, presses Enter, daemon's response streams to the TUI in real time.

**AC-3.2** Slash commands: `/clear`, `/help`, `/quit`, `/workstream list`, `/workstream activate <id>` all work.

**AC-3.3** Scrollback: Up-arrow recalls history; Page-up/down, mouse-wheel scroll the chat.

**AC-3.4** Line editing: Ctrl-a (start), Ctrl-e (end), Ctrl-u (clear input), Ctrl-c (cancel), Ctrl-d (quit), Backspace works.

**AC-3.5** Workstream awareness: status line shows active workstream; `/workstream activate <id>` updates display on daemon response.

**AC-3.6** Connection resilience: daemon disconnect shows "Connection lost" status; next input attempt reconnects.

**AC-3.7** Tool invocations are rendered as text (e.g., `[TOOL] git.checkout: main`).

### AC-4: Code quality

**AC-4.1** All public APIs are documented with doc comments.

**AC-4.2** Unit tests cover ReplApp state transitions, event dispatch, line editing.

**AC-4.3** Integration tests verify CodeEngine + mock daemon work end-to-end.

**AC-4.4** sld-lint passes on this spec without errors or warnings.

### AC-5: Phasing adherence

**AC-5.1** MVP ships all Slices 1–7 and 10 (framework, event loop, CodeEngine, ReplApp, line editing, workstream awareness, slash commands, tagent refactoring).

**AC-5.2** Phase 2 ships Slices 8–9 (tool cards, permission prompts).

**AC-5.3** Each slice is independently shippable and tested.

---

## 8. Non-goals {#SPEC-TTUI-08~draft}

**ID:** SPEC-TTUI-08~draft
**Status:** Draft

1. **Not a replacement for the web/Tauri GUI.** The SPA (DOC-39) is the primary UI; tcode-tui is an alternative entry point.
2. **Not SSH-hardened in MVP.** Phase 2 adds fallback; MVP assumes a full-size terminal.
3. **Not feature-complete.** MVP is the core loop; permission prompts, fancy tool cards, workstream-creation UI are Phase 2+.
4. **Not a fork of tagent's REPL.** Extraction and shared consumption are the goal; no duplication.
5. **Not pixel-perfect parity with Claude Code.** Functional parity in interaction model is the goal.
6. **Not a replacement for the CLI client.** The one-shot `tcode run-task` CLI remains; tcode-tui is an interactive alternative.
7. **Not a general-purpose IDE.** The Project/IDE half (DOC-39 §7 Q4) remains out of scope.

---

## 9. Relationship to other specs

| Spec | Relationship |
|---|---|
| DOC-39 (trusty-code Harness UI) | This spec is the TUI implementation of DOC-39's layer-priority axiom and thin-client constraint. [`SPEC-TCUI-01~draft`](./trusty-code-harness-ui.md#SPEC-TCUI-01~draft) and [`SPEC-TCUI-09~draft`](./trusty-code-harness-ui.md#SPEC-TCUI-09~draft) are the binding constraints. |
| DOC-48 (tcode Workstreams) | This spec integrates workstream awareness into the TUI. The TUI surfaces the active workstream and responds to `WorkstreamActivationChanged` events (DOC-48 §5.3). |
| DOC-38 (Spec-Linked Documentation) | This spec follows [`SPEC-SLD-02~draft`](./spec-linked-documentation.md#SPEC-SLD-02~draft) reference grammar and conventions. |

---

## 10. Glossary

| Term | Definition |
|---|---|
| **Engine adapter** | A thin implementation of `TuiEngine` trait for a product (tcode or tagent). Translates user input into daemon calls and daemon responses into TUI events. |
| **Thin client** | A UI that renders daemon state and issues daemon calls; no local business logic or filesystem access. Binding constraint from DOC-39. |
| **ReplEvent** | A union type (enum) for all events in the TUI loop: key presses, resize, assistant output, tool invocations, status messages, workstream updates. |
| **ReplApp** | The state machine for the REPL: chat scrollback, input buffer, history, cursor position, tick counters, active workstream. |
| **Slash command** | A user command starting with `/`, routed to the engine for handling (e.g., `/help`, `/model`, `/workstream activate`). |
| **Workstream** | A named, durable grouping of sessions (DOC-48 §2.1). The TUI shows the active workstream and can switch between them. |

---

## 11. Verification and test plan

### Manual testing (MVP)

1. **Launch tcode TUI:** `tcode tui --project /path/to/project`
2. **Type a prompt:** "Write a function to reverse a string in Python"
3. **Verify streaming:** Response appears in real time, character by character.
4. **Verify history:** Press Up-arrow, prior prompt recalled.
5. **Verify workstream:** Status line shows active workstream; run `/workstream activate <id>`, see status line update.
6. **Verify slash commands:** `/help` shows commands; `/clear` clears scrollback; `/quit` exits.
7. **Verify tool invocation:** Trigger a tool call (e.g., `tcode tui` with a task that calls `fs.read`), see `[TOOL] fs.read: …` in the scrollback.
8. **Verify connection loss:** Kill the daemon, continue typing in the TUI, see "Connection lost", restart daemon, next input reconnects.

### Automated testing

- **Unit tests:** ReplApp state transitions, line editing, event dispatch.
- **Integration tests:** CodeEngine + mock daemon, full event loop end-to-end.
- **Regression tests:** Tagent's REPL works identically after refactoring.

### Lint and build

```bash
cargo build -p trusty-tui --release
cargo test -p trusty-tui --release
cargo test -p trusty-code --release
cargo test -p trusty-agents --release
bash scripts/check_sld.sh    # SLD spec-doc lint (this spec)
```

---

## 12. Future roadmap (Phase 3+)

- **Suggested next prompts** (#2078): Claude-Code-style suggestions after each response.
- **Workstream creation UI:** Modal to mint a new workstream and bind it to a project.
- **Inline file browser:** Quick file/directory picker (context: #3405 project picker modal, DOC-48 Phase C+).
- **Per-workstream settings:** Remember model/agent choices per workstream.
- **Plugin/extension system:** Allow engines to register custom widgets or commands.
