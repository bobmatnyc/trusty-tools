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
- [`docs/trusty-code/vision-and-architecture-spec.md`](../trusty-code/vision-and-architecture-spec.md) — Axiom §1 reserves the TUI layer as a "future layer" and foundation for interactive use. (Note: DOC-39 §1.2 explicitly states the Platform is "an SPA (web/Tauri)… Not a TUI," positioning DOC-50 as a secondary/alternative entry point, not the primary platform. See §9 Q8.)
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

**trusty-code has no TUI today, by design.** The one-shot `tcode run-task` CLI (in `crates/trusty-code/src/cli_client/`) is the current entry point. The vision-and-architecture spec reserves the TUI layer as a future interactive surface; this spec is that implementation. **Authorization:** DOC-39 §1.4 (amended in this PR) formally acknowledges the TUI as a **sanctioned secondary/alternative entry point** — the primary Platform remains the SPA ("Not a TUI" refers to the primary platform). The TUI is an additional thin client over the same daemon API, not a competing platform.

**The goal:** Build an **interactive terminal UI that mimics Claude Code's core UX** — streaming assistant output, tool-call rendering, an input composer, slash commands, scrollback, status line, and workstream awareness — while strictly adhering to DOC-39's **thin-client axiom** (§2.1): the TUI communicates with the daemon over HTTP (long-lived, SSE-enabled); all agent logic stays server-side in the daemon.

**What "Claude Code clone" means here:**
- **Streaming input/output:** Assistant responses stream to the TUI; the user sees partial output in real time.
- **Tool-call rendering:** When an agent invokes a tool (e.g., `fs.read`, `git.checkout`), the TUI renders the call and its result (as cards or inline, depending on phase).
- **Input composer:** A multi-line input prompt at the bottom, with history (Up-arrow), line editing, and in-flight cancel (Ctrl-C).
- **Slash commands:** `/help`, `/clear`, `/model`, `/agent`, `/workstream`, etc. — routed through the engine adapter to the daemon or handled client-side.
- **Scrollback:** Mouse-wheel scroll, page-up/down, history recall. Rendering is diff-based for performance.
- **Workstream awareness:** The TUI shows the active workstream name/ID and participates in activation events (DOC-48 §5.3).
- **Status line:** Session ID, current model, project name, workstream state, tmux session count (if observed). *(Daily cost display: out of scope for MVP pending daemon API support; see §9 Q9.)*
- **Permission/diff prompts:** Future phases — the TUI surfaces permission gates and diffs for code changes (part of DOC-39 Phase 2).

**Non-requirement:** This spec does NOT aim for pixel-perfect parity with Claude Code's visual design. The goal is **functional parity** in interaction model and information architecture.

### 1.2 Non-goals

1. **Not the primary platform.** DOC-39 §1.2 commits the platform to an SPA (web + Tauri shell); this TUI is a *secondary/alternative* entry point for terminal-native users. Both coexist; the SPA is the primary interactive surface.
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

**Connection loss (HTTP/SSE semantics):** CodeEngine communicates with the daemon over HTTP (req/resp for RPC calls) and SSE subscriptions (workstream events). When the daemon connection is lost:
- **HTTP req/resp:** new input triggers a POST /rpc call; if daemon is unreachable (502/503 or timeout), CodeEngine returns error; TUI shows "Connection lost — attempting reconnect…"
- **SSE subscription (workstream events):** long-lived stream closes on daemon restart (HTTP 503 or connection drop); CodeEngine re-subscribes to the same /workstreams/{ws_id}/events endpoint (automatic reconnect per tokio/reqwest behavior).
- **User-facing:** TUI input loop remains live; user can continue typing; next RPC call or SSE resubscription succeeds once daemon restarts. On daemon restart, TUI may need to refetch workstream state (fresh RPC call to `workstream.get`).

**Panic safety:** The `setup_terminal`/`restore_terminal` RAII guard (Drop impl) ensures that even if the TUI or event loop panics, the terminal is restored to cooked mode (not corrupted with alt-screen left on).

**Testability:** The `TuiEngine` trait allows tests to mock the daemon. A test engine can return fixed responses without spawning a real HTTP daemon (simple mock responder).

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

### 3.2 Step 2: Generalization layer (ReplApp → engine-supplied data)

**Critical finding:** `ReplApp` (from tagent's types.rs:78+) currently interleaves generic UI fields with ~15 tagent-specific ones (tm_session_count, claude_mpm_session_count, local_model, daily_cost_start, usage_project_dir, model_name, provider_name). The statusline hardcodes OpenRouter pricing (status.rs:145–155). Pickers and slash commands are likewise hardcoded to tagent's needs.

**Fix (Slice 1.5 — INSERT between Slice 1 and Slice 2):** Explicitly split `ReplApp` into:
- **Engine-agnostic core:** chat buffer, input state, history, cursor, tick counters.
- **Engine-supplied adapters:** statusline segments (enum-driven, engine-populated), picker data sources (engine callback), slash-command registry (engine-provided).

**Result:** Widgets and state machine remain UI-generic; all semantic data flows from the engine. This is a prerequisite for Slice 4+ to work without tagent/tcode divergence.

### 3.3 Step 3: Scaffold trusty-tui (framework + trait)

**Phase 1A.1:** Create `trusty-tui` crate (new).

**Phase 1A.2:** Scaffold only the **framework** (terminal setup/restore RAII, TuiEngine trait, ReplEvent enum, event_loop skeleton) and move/adapt tests. No widgets yet.

### 3.4 Step 4: Implement CodeEngine (tcode engine adapter)

**Phase 1A.3:** New file: `crates/trusty-code/src/tui_client/engine.rs`

**RESOLVED (Q7): HTTP Long-lived Daemon Transport**

The CodeEngine communicates with a long-lived `tcode serve --http` daemon (default port 7881, per serve/mod.rs:70). Workstream awareness uses DOC-48 §5.3 SSE `WorkstreamActivationChanged` events.

**Daemon Discovery Pattern (matching trusty-search/trusty-memory precedent):**

1. **Discovery file location:** `~/.trusty-code/daemon.json` (or environment var `TCODE_DAEMON_URL`)
   - File format: `{"daemon_url": "http://localhost:7881", "pid": 12345, "started_at": "2026-07-20T..."}`
   - Purpose: allows `tcode tui` and `tcode run-task` to find the running daemon without port scanning.

2. **Daemon lookup sequence:**
   - Check env var `TCODE_DAEMON_URL` (highest priority).
   - Read `~/.trusty-code/daemon.json`; validate daemon is alive (http GET /?ping or HEAD / → 200).
   - If not alive or missing, skip (Phase 2: auto-spawn or fallback).
   - For MVP: bail with "No daemon found; start one with `tcode daemon --project <path>`" (documented limitation).

3. **CodeEngine struct:**
   ```rust
   pub struct CodeEngine {
       project_path: PathBuf,
       daemon_url: String,        // e.g., "http://localhost:7881"
       http_client: HttpClient,   // reqwest or similar
       session_id: Option<String>,
   }
   ```

4. **Methods:**
   - `new(project_path: PathBuf) -> Result<Self>` — discover daemon, return CodeEngine or error.
   - `handle_input(line: String, tx: UnboundedSender<ReplEvent>) -> Result<bool>` — parse slash cmd vs. chat; call daemon; stream responses as ReplEvent.
   - `setup(tx: UnboundedSender<ReplEvent>) -> Result<()>` — POST /rpc `session.create`, fetch initial workstream, emit ReplEvent::WorkstreamUpdated.
   - `cancel_session() -> Result<()>` — POST /rpc `session.cancel` (for Ctrl-c in Slice 5).
   - `subscribe_workstream_events(ws_id: UUID) -> Result<EventStream>` — GET /workstreams/{ws_id}/events (SSE), spawn listen task, emit ReplEvent on changes.

5. **Transport semantics:**
   - HTTP requests use pooled client (keep-alive).
   - SSE subscriptions are long-lived (single reconnect per workstream activation change).
   - Reconnect on 502/503 (daemon restart) or timeout; user sees "Connection lost" status.
   - No local `--stdio` child spawned (daemon is independent).

**MVP Known Limitation:** "tcode tui assumes a running `tcode serve --http` daemon on localhost:7881; see `tcode daemon --project <path>` to start one." Discovery/auto-spawn is Phase 2.

### 3.5 Step 5: Add widgets and render layer (trusty-tui)

**Phase 1A.4:** Move widgets (scrollback, input_composer, status_line, pickers) and layout from tagent to trusty-tui/src/widgets/. Tests must migrate (no coverage drops).

### 3.6 Step 6: Refactor tagent onto trusty-tui (ATOMIC CUTOVER)

**Phase 1A.5:** Create `AgentEngine` impl in tagent. Refactor tagent's REPL to use `run_tui<AgentEngine>`.

**CRITICAL — Atomicity:** All file deletions from tagent/src/repl/tui/, the new AgentEngine impl, the new trusty-tui dependency, and the compat-shim (if any) MUST land in the SAME commit. Tagent's REPL must never be broken on main (no intermediate state where files are deleted but engine is not wired).

**Regression test:** Tagent REPL behavior unchanged (minus panic-safety improvement; see Slice 2 AC).

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

### Slice 1.5: Generalization layer — ReplApp engine-supplied data (Phase 1A.2)

**What:** Separate engine-agnostic UI state from engine-supplied data (statusline segments, picker data, slash-command registry).

**Deliverable:**
- Refactor `ReplApp` to expose engine-provided callbacks/adapters for statusline content, picker data sources, and command registry.
- Define `StatuslineSegment` enum (engine-populated); statusline widget consumes it (not hardcoded).
- Define `SlashCommandRegistry` trait; engine provides the command table (not a hardcoded array).
- Define `PickerDataSource` trait; pickers query it dynamically (not hardcoded lists).
- Migrate tagent's hardcoded pricing, model/provider pickers, command array into AgentEngine-supplied data (deferred to Slice 10, but interface exists now).

**Acceptance:**
- `ReplApp` compiles and has no tagent/tcode-specific imports.
- Tests verify engine can supply arbitrary statusline segments and command registries.
- Widgets consume engine data, not hardcoded values.

**Effort:** 1 engineer-day.

### Slice 2: Terminal setup, event loop, and ratatui 0.30 spike (Phase 1A.2)

**What:** Implement the panic-safe terminal RAII guards, event channel, key reader thread, 100ms-tick loop, and validate ratatui 0.30 compatibility.

**Deliverable:**
- `setup_terminal()` → Terminal with alt-screen + raw mode + mouse capture.
- `restore_terminal()` RAII guard implemented as Drop trait (panic-safe: terminal is restored even if event loop panics).
- Key reader thread spawned; KeyEvent routed to mpsc channel.
- 100ms-tick interval with backpressure note (see §10 below).
- `event_loop<H: TuiEngine>` scaffold: `tokio::select!` pattern, tick + rx.recv() branches, process ReplEvent, call `engine.handle_input()`, loop on `app.quit`.
- **Spike:** Compile tagent's chat.rs, markdown.rs, banner.rs, and layout code against ratatui 0.30 (before moving them in Slice 4). Verify no breaking API changes; document any migration steps. This de-risks the 0.30 bump (Q2 resolution).

**Acceptance:**
- `cargo run -p trusty-tui --example minimal` enters alt-screen; panic in event loop triggers Drop guard → terminal restored to cooked mode.
- Key presses captured and routed.
- Tick fires every 100ms.
- Ratatui 0.30 spike compiles; breaking changes (if any) documented.
- **Behavioral change from tagent:** Tagent's REPL now gains panic-safety (previously, a panic left the terminal in raw/alt-screen mode). This is a bug fix, not a regression.

**Effort:** 1.5 engineer-days.

### Slice 3: CodeEngine implementation (Phase 1A.3, BLOCKED ON Q7)

**What:** Implement `CodeEngine` (the tcode engine adapter) against a determined transport (see §9 Q7).

**Deliverable:**
- `crates/trusty-code/src/tui_client/engine.rs`: `CodeEngine` struct, `TuiEngine` impl.
- `handle_input`: parse slash commands vs. chat, call daemon, stream responses as `ReplEvent::AssistantOutput`.
- `setup`: fetch initial workstream, session ID, etc.
- `cancel_session()`: call `session.cancel` RPC on the daemon (for Ctrl-c in Slice 5).
- Transport is determined by Q7 resolution: **--stdio** (no SSE, workstream awareness polls) or **--http** (SSE-enabled, discovery TBD).
- Error recovery: auto-reconnect on daemon loss.

**Acceptance:**
- Code compiles (transport-agnostic to B or A in Q7).
- Integration test: spawn mock daemon, send prompt, verify ReplEvent emitted.
- Cancel test: verify `cancel_session()` calls daemon RPC.

**Effort:** 1 engineer-day (plus design time awaiting Q7 decision).

**BLOCKER NOTE:** This slice depends on resolving §9 Q7 (transport choice). Slice 6 (workstream awareness) also depends on it (SSE vs polling).

### Slice 4: ReplApp state, widgets, and tagent file migration (Phase 1A.4)

**What:** Move `ReplApp` and widgets from tagent to trusty-tui; migrate tests; inventory/migrate orphaned files.

**Deliverable:**
- `trusty_tui::ReplApp` struct (generalized per Slice 1.5): chat messages, input buffer, history, cursor, tick counter, engine-supplied adapters.
- Widgets: scrollback (Paragraph + line wrapping), input_composer (Block + editable), status_line (Gauge + engine-supplied Spans).
- `draw(Frame, ReplApp)` function: renders widgets.
- **File migration from tagent/src/repl/tui/ to trusty-tui/src/:**
  - `chat.rs` (454L) → `trusty-tui/src/widgets/chat.rs` (or delete if subsumed by scrollback.rs + input_composer.rs).
  - `markdown.rs` (297L) → `trusty-tui/src/render/markdown.rs` (formatting helper).
  - `banner.rs` (247L) → `trusty-tui/src/widgets/banner.rs` (or Phase 2, status quo TBD).
  - `tests_render.rs, tests_input.rs, tests_state.rs` (~1,436L total) → **MUST MIGRATE** to trusty-tui/ (no coverage drops). Create `trusty-tui/src/tests/` with equivalent structure.
  - Spike output (Slice 2) informs any ratatui 0.30 fixes needed during migration.

**Acceptance:**
- `cargo test -p trusty-tui` passes (all tagent tests now trusty-tui tests).
- Visual test: `tcode tui` renders scrollback, input, status line correctly.
- All orphaned files have a home; none left behind without justification.

**Effort:** 2 engineer-days (file migration is careful work, tests must not regress).

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
- Ctrl-c → cancel in-flight request: calls `engine.cancel_session()` which relays a daemon `session.cancel` RPC call (per DOC-39 C-2, thin-client axiom — the daemon must perform the actual cancellation, not just the UI's render stop). Blocks user input until cancel completes.
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

## 6. Open Questions for Bob — All Resolved (2026-07-20) {#SPEC-TTUI-06~resolved}

**ID:** SPEC-TTUI-06~resolved
**Status:** Resolved (Bob sign-off 2026-07-20)

### Q1 — Shared crate naming

**RESOLVED (Bob, 2026-07-20):** New standalone `crates/trusty-tui/` crate (not a trusty-common feature).

Clear isolation; ratatui + crossterm are heavy deps unrelated to trusty-common's core. Independent versioning supports future harnesses.

### Q2 — Ratatui 0.30 adoption timing

**RESOLVED (Bob, 2026-07-20):** Adopt ratatui 0.30 now. Slice 2 includes 0.30 compatibility spike.

Removes vulnerable `lru` transitive (#2886). Spike de-risks incompatibilities before widget migration (Slice 4).

### Q3 — SSH/narrow-terminal fallback timing

**RESOLVED (Bob, 2026-07-20):** Phase 2 (not MVP).

Known limitation: "tcode tui requires full-size terminal with alt-screen support; SSH/narrow terminals use `tcode run-task` CLI."

### Q4 — Engine adapter flexibility (slash command routing)

**RESOLVED (Bob, 2026-07-20):** Mixed routing (Option 2).

Built-in commands (`/help`, `/clear`, `/quit`) are client-side (trusty-tui). Domain-specific commands (`/model`, `/agent`, `/workstream`) route to engine. Keeps MVP simple.

### Q5 — Workstream activation in MVP

**RESOLVED (Bob, 2026-07-20):** Text command (`/workstream activate <id>`) in MVP; visual switcher Phase 2.

Text command sufficient for MVP scope. Visual switcher (header dropdown) is Phase 2 UX refinement.

### Q6 — Model/agent pickers: inline or modal?

**RESOLVED (Bob, 2026-07-20):** Inline list (below input), not modal.

Matches tagent's familiar picker style; simpler to implement. Modal is a future visual-design change if needed.

### Q7 — Daemon transport: --stdio vs --http

**RESOLVED (Bob, 2026-07-20):** `tcode serve --http` long-lived daemon (port 7881). UNBLOCKS Slices 3 and 6.

Workstream awareness (MVP Slice 6) uses DOC-48 §5.3 SSE `WorkstreamActivationChanged` events over HTTP. CodeEngine sketch (§3.3) now concrete: defines HTTP transport with daemon discovery pattern (see below).

### Q8 — DOC-39 amendment needed?

**RESOLVED (Bob, 2026-07-20):** Yes. Amend DOC-39 to acknowledge the TUI as a sanctioned SECONDARY / alternative entry point.

**Action taken in this branch:** Added subsection to DOC-39 (same PR #3409) with proper {#SPEC-...} anchor, acknowledging DOC-50 as SECONDARY thin client. Primary Platform remains SPA ("Not a TUI" still holds). Cross-references updated; DOC-39 sld-lint remains clean.

### Q9 — Daily cost display: MISSING daemon API or drop?

**RESOLVED (Bob, 2026-07-20):** Flag as MISSING daemon-API gap (per DOC-39 C-2); deferred to Phase 2.

Removed from MVP status line (shows session/model/project/workstream only). Cost display pending daemon RPC support (e.g., `session.get_cost()`).

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

**AC-2.3** Tagent's REPL is refactored to use `trusty-tui` with **UX parity + panic-safety improvement**. All existing commands, scrollback, history, and line editing work identically. **Behavioral change (bug fix):** Tagent gains panic-safe terminal restoration (RAII Drop guard); a panic in the event loop no longer leaves the terminal in raw/alt-screen mode (tagent's current bug, fixed by Slice 2).

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

**AC-5.1** MVP ships all Slices 1–7 and 10 (framework, generalization layer, event loop + panic-safety + 0.30 spike, CodeEngine [blocked on Q7], generalized ReplApp + widgets + test migration, event dispatch + Ctrl-C daemon cancel, workstream awareness + SSE, slash commands, tagent refactoring). Slice 3/6 are blocked on Q7 resolution.

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

## 11a. Event channel backpressure note

The ReplEvent channel feeding the event loop is an **unbounded mpsc::unbounded_channel** (no flow control). At 100ms tick intervals and typical TUI event rates (key presses, key reader thread emissions), this is safe for MVP. **If streaming daemon responses significantly outpace the 100ms render loop, backlog can grow unbounded.** A future optimization (Phase 2+) may adopt a bounded channel with drop-oldest or a ring buffer (ringbuf-like semantics). For now, log cumulative backlog depth at startup and in tests; flag if > 1000 events queued.

---

## 12. Future roadmap (Phase 3+)

- **Suggested next prompts** (#2078): Claude-Code-style suggestions after each response.
- **Workstream creation UI:** Modal to mint a new workstream and bind it to a project.
- **Inline file browser:** Quick file/directory picker (context: #3405 project picker modal, DOC-48 Phase C+).
- **Per-workstream settings:** Remember model/agent choices per workstream.
- **Plugin/extension system:** Allow engines to register custom widgets or commands.
