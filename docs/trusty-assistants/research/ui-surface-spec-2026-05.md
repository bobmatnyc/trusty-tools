# UI Surface Specification — open-mpm

**Version**: 1.0  
**Date**: 2026-05-02  
**Status**: Canonical reference  
**Audience**: Engineers implementing or modifying any open-mpm UI surface

---

## 1. Overview

### Purpose

This document is the authoritative specification for all user-facing UI surfaces in open-mpm. It defines what each surface must render, when, and how — with enough precision that engineers can implement or modify any area without further design discussion.

### Surfaces in Scope

| ID | Surface | Technology | Primary Persona |
|----|---------|------------|-----------------|
| A | TUI | ratatui (terminal) | Power user, developer |
| B | Web/GUI | Tauri + Svelte | Non-terminal user, project manager |
| C | Messaging | Telegram bot | Mobile / async user |

### Out of Scope

- Headless / API-only clients (no rendering obligations)
- CLI one-shot mode (`cargo run -- --task "..."`)
- Future surfaces (VS Code extension, Slack bot) — these surfaces inherit this spec's canonical area definitions but require their own implementation appendix

---

## 2. Canonical Area Definitions

The following seven areas constitute the complete UI vocabulary for open-mpm. Every surface implements a subset of these areas. Where a surface cannot render an area (e.g., Telegram has no persistent statusline), the spec defines an acceptable substitute.

| # | Area | Purpose | When Shown | Interaction Model | Update Trigger |
|---|------|---------|------------|-------------------|----------------|
| 1 | **prompt-input** | User composes and submits a message | Always | Text entry; submit with Enter (or send button); slash commands expand inline | User keystroke |
| 2 | **active-prompt** | Echoes the submitted message while response is in-flight | From submit until response complete | Read-only; differentiates in-flight state from history | Submit action |
| 3 | **active-response** | Shows the in-progress or streaming response from the PM | While session is running | Read-only; updated per event or per token chunk | `AgentMessage`, `task-progress`, final `LlmResponded` |
| 4 | **history** | Scrollable log of all past exchanges in the session | Always (empty until first exchange) | Scroll; optionally search; read-only | `SessionDone`, `task-complete` |
| 5 | **subagent-activity** | Spinner, agent names, elapsed time, step labels during delegation | Only while a session is active (`SessionStarted` → `SessionDone`) | Read-only; collapses when idle | `PmDelegating`, `AgentSpawned`, `AgentStarted`, `AgentDone`, `AgentFailed`, `PhaseStarted`, `PhaseDone`, `PhaseSkipped`, `PersonaDetected` |
| 6 | **tool-calls** | Display of tool invocations (name, args preview, result preview) | Only while a session is active, or on demand in history | Collapsible; read-only; expandable for full args/result | `ToolCalled`, `ToolResult` |
| 7 | **statusline** | Ambient ambient information: model, provider, tokens, cost, connection | Always | Read-only; updates in background | `LlmRequested`, `LlmResponded`, session lifecycle |

### Canonical Terminology

- **Session**: A single user prompt → PM orchestration → response cycle. Identified by `session_id`.
- **Project**: A directory connected to a PM agent. A user can switch between projects. Multiple sessions belong to one project.
- **Scope**: `User` (ctrl/persona agent, cyan) or `Project` (connected PM, yellow). Displayed in TUI label color and sidebar icon.
- **Runner type**: How an agent is dispatched — `subprocess`, `claude-code`, or `inline`.

---

## 3. TUI Surface Spec (Surface A)

### 3.1 Layout Diagram

```
┌──────────────────────────────────────────────────────────────────────────┐
│  BANNER (12 rows, hidden after first chat entry)                         │
│  [logo + scope label (left)] [git commits + command hints (right)]       │
├──────────────────────────────────────────────────────────────────────────┤
│  HISTORY (scrollable, fills remaining vertical space)                    │
│  ❯ user message                                          (green, bold)   │
│  ⏺ assistant response                                    (white)         │
│  ✗ error message                                         (red)           │
│    status/system line                                    (dim italic)    │
├──────────────────────────────────────────────────────────────────────────┤
│  SUBAGENT-ACTIVITY (3 rows, only while busy)                             │
│  ✻ Processing… (Xs · ↑N ↓N · $X · thinking)             (yellow spinner)│
│    ↳ anthropic/claude-sonnet-4-6                         (dim italic)    │
│    ↳ latest ThinkingStep text                            (dim italic)    │
├──────────────────────────────────────────────────────────────────────────┤
│  ACTIVE-RESPONSE PREVIEW (1 row, always present)                         │
│  ❯ Press up to edit queued messages  [idle]                              │
│  ❯ <streaming preview text>           [busy]                             │
├──────────────────────────────────────────────────────────────────────────┤
│ ──────────────────────────────────────────────────── separator ──────── │
│  PROMPT-INPUT (1 row)                                                    │
│  ❯ _  (cyan = user scope / yellow = project scope)                       │
├──────────────────────────────────────────────────────────────────────────┤
│ ──────────────────────────────────────────────────── separator ──────── │
│  STATUSLINE (1 row)                                                      │
│  [open-mpm] ✓ LLM: openrouter (claude-sonnet-4-6) · ↑N ↓N · $X · ...  │
└──────────────────────────────────────────────────────────────────────────┘
```

Minimum terminal width: 80 columns. Layout degrades gracefully at 80 cols by truncating statusline segments right-to-left (cost → tokens → git → model abbreviation).

### 3.2 Per-Area Target State and Acceptance Criteria

#### prompt-input

**Target state**: Single-line input with ratatui cursor. Prefix glyph color encodes scope (cyan = User/ctrl, yellow = Project/PM). Slash commands are parsed client-side before submission.

**Acceptance criteria**:
- Cursor renders at exact `cursor_pos` via `set_cursor_position`.
- `Ctrl-A` / `Ctrl-E` move to start/end of line.
- `Ctrl-U` clears the line.
- `Ctrl-C` cancels current input (does not submit).
- `Ctrl-D` on empty line quits the REPL.
- Up-arrow recalls previous submitted prompt; additional up presses cycle through `repl_history`.
- Down-arrow moves forward in history; reaching the end restores the current draft.
- Slash commands beginning with `/` are handled before LLM submission (see Section 3.4).
- Input is disabled (no keystrokes processed) while a session is active.

#### active-prompt

**Target state**: Submitted user line is immediately appended to history scrollback with `❯` prefix (green, bold) before the LLM call begins. No separate "in-flight" style — the history entry is permanent.

**Acceptance criteria**:
- User line appears in history within one render frame of Enter.
- The input row is cleared immediately on submit.
- The prompt glyph color on the history entry matches the scope at time of submission (not current scope).

#### active-response

**Target state**: While a session is running, `ThinkingStep` events appear as dim italic status lines below the last user entry in the history area. The `streaming_preview` row (area above separator) shows the most recent step text. When `SessionDone` fires, the full response is pushed to history as an `Assistant` or `Error` entry and the status lines are cleared.

**Acceptance criteria**:
- Each `ThinkingStep` appears within two render frames of the event.
- The preview row updates to the latest step text; does not scroll the full history.
- On `SessionDone` with `status = "success"`: the full PM response replaces the status lines as an `Assistant` (white, `⏺`) history entry.
- On `SessionDone` with `status = "error"`: response rendered as `Error` (red, `✗`) history entry.
- Status lines (dim italic) are removed from the scrollback once the final response is appended.

#### history

**Target state**: Scrollable `Vec<ChatLine>` with roles User / Assistant / Error / Status. No hard cap. REPL input history persisted to `~/.open-mpm/repl_history.txt`.

**Acceptance criteria**:
- `PageUp` / `PageDown` scroll by 10 lines.
- `Home` / `End` jump to top / bottom.
- Auto-scrolls to bottom on new message (unless user has scrolled up manually; if scrolled up, a "N new messages" indicator appears at bottom).
- Each `Assistant` entry prefixed with `⏺` (white).
- Each `User` entry prefixed with `❯` (green, bold).
- Each `Error` entry prefixed with `✗` (red).
- `Status` entries rendered dim italic, no prefix glyph.
- Banner collapses (hidden, rows reclaimed) as soon as any `ChatLine` exists.
- Repl input history survives process restart.

#### subagent-activity

**Target state**: 3-row panel, visible only while `busy_since` is set (a session is active). Collapses entirely (zero rows, no separator) when idle. Content:

- Row 1: `✻ Processing… (Xs · ↑N ↓N · $X · <verb>)` where `<verb>` cycles among "thinking" / "working" / "processing" every 500ms. `X` = wall-clock seconds since `SessionStarted`. `↑N ↓N` = cumulative session tokens. `$X` = cumulative session cost.
- Row 2: `↳ <model>` from the most recent `LlmRequested` event (dim italic).
- Row 3: Latest `ThinkingStep` text (dim italic), truncated to terminal width.

**Acceptance criteria**:
- Panel appears within one render frame of `SessionStarted`.
- Panel disappears within one render frame of `SessionDone` or `SessionCancelled`.
- `✻` spinner glyph animates (cycles through `⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏` at 100ms or equivalent) while busy.
- Elapsed time counter increments every second.
- Token and cost counters update on every `LlmResponded` event.
- `AgentSpawned` event updates row 2 model to the spawned agent's model (if known).
- `PersonaDetected` prepends persona name to row 3: `[hacker] ↳ latest step`.
- `PhaseSkipped` renders row 3 as: `⊘ <phase> skipped (<persona>)` (dim italic).

#### tool-calls

**Target state** (Phase 1): Tool invocations appear as `ThinkingStep` text in the subagent-activity panel and as `Status` history lines. Format: `  ⤳ <tool>: <preview>` for `ToolCalled`; `  ✓ <tool>: <preview>` for `ToolResult` success; `  ✗ <tool>: <preview>` for error.

**Target state** (Phase 4, deferred): Collapsible structured cards in history. Each card shows tool name, collapsed args preview, and expandable full JSON on Enter.

**Acceptance criteria (Phase 1)**:
- Every `ToolCalled` event produces a `Status` history line visible in the scrollback.
- Every `ToolResult` event produces a corresponding `Status` history line.
- Tool lines are dim italic, indented two spaces relative to surrounding text.
- `preview` field is truncated to 120 characters if longer.

#### statusline

**Target state**: Single row at the very bottom. Segments (left to right):

1. `[open-mpm]` — cyan bold, always shown
2. `✓ LLM: <provider> (<model>)` — green tick; tick turns red (`✗`) if last LLM call returned an error
3. ` · ↑N ↓N` — cumulative session prompt + completion tokens
4. ` · $X.XXX` — cumulative session cost
5. ` · <git-branch>*` — current git branch; `*` if dirty (shown only when in a project directory)
6. ` · <statusline-message>` — "All systems go." when healthy; error text when unhealthy

Segments 3–5 appear only after the first `LlmResponded` event in the session. Segment 5 appears only when `git_branch` segment is enabled in config.

**Acceptance criteria**:
- All segments render on a single row; truncation is right-to-left when terminal width is insufficient.
- Token and cost values update within one render frame of each `LlmResponded`.
- Model/provider updates within one render frame of each `LlmRequested`.
- `StatuslineConfig` in `.open-mpm/config.toml` controls which segments are shown; defaults include model, provider, tokens, cost.

### 3.3 Keyboard Bindings Reference

| Key | Action |
|-----|--------|
| `Enter` | Submit prompt |
| `Shift+Enter` | Insert newline (multi-line input, if supported) |
| `Up` | Recall previous repl history entry |
| `Down` | Move forward in repl history |
| `PageUp` | Scroll history up 10 lines |
| `PageDown` | Scroll history down 10 lines |
| `Home` | Jump history to top |
| `End` | Jump history to bottom |
| `Ctrl-A` | Move cursor to start of input line |
| `Ctrl-E` | Move cursor to end of input line |
| `Ctrl-U` | Clear input line |
| `Ctrl-C` | Cancel current input (if idle); cancel in-flight session (if busy) |
| `Ctrl-D` | Quit REPL (only on empty input line) |
| `Ctrl-L` | Clear history display (does not affect persisted history) |
| `Esc` | Dismiss modal overlay (picker, projects view) |
| `Up` / `Down` (in picker) | Navigate picker rows |
| `Enter` (in picker) | Confirm picker selection |
| `F2` | (Future) Toggle sessions/projects view |

### 3.4 Slash Command List

| Command | Action |
|---------|--------|
| `/help` | Print command list as Status lines in history |
| `/clear` | Clear history display and reset session context |
| `/model` | Open model picker overlay |
| `/provider` | Open provider picker overlay |
| `/agent <name>` | Set active sub-agent for next delegation |
| `/connect <path>` | Connect to a project PM at `<path>`; updates scope to Project (yellow) |
| `/disconnect` | Disconnect from current project; return to User scope (cyan) |
| `/projects` | Open full-screen projects picker (see Section 6.1) |
| `/status` | Print current model, provider, tokens, cost, project path as Status lines |
| `/tools` | Print recent tool calls for current session as Status lines |
| `/workflow <name>` | Run a named workflow from `.open-mpm/workflows/` |
| `/version` | Print harness version as Status line |
| `/quit` | Quit REPL |

---

## 4. Web/GUI Surface Spec (Surface B)

### 4.1 Layout Diagram

```
┌──────────────────┬─────────────────────────────────────────────────────┐
│  SIDEBAR (288px) │  CHAT HISTORY (flex-grow, scrollable)               │
│                  │                                                       │
│  [Logo]   [●]    │    ┌─────────────────────────────────────────────┐  │
│                  │    │  user bubble (right, gray bg, rounded)       │  │
│  Projects        │    └─────────────────────────────────────────────┘  │
│  ● project-a     │    ┌──────────────────────────────────────────────┐ │
│  ○ project-b     │    │ [pm] assistant bubble (left, teal border)    │ │
│  ● project-c     │    │                                              │ │
│                  │    │  SUBAGENT-ACTIVITY (inline, collapsible)     │ │
│  [dashboard row] │    │  ┌──────────────────────────────────────┐   │ │
│                  │    │  │ ✻ Delegating to engineer… (3s)        │   │ │
│  Task History    │    │  │ ↳ claude-sonnet-4-6                   │   │ │
│  [task list]     │    │  └──────────────────────────────────────┘   │ │
│                  │    │                                              │ │
│                  │    │  TOOL-CALLS (inline, collapsible per tool)  │ │
│  [New Project]   │    │  ▶ delegate_to_agent  preview…             │ │
│  [Theme Toggle]  │    └──────────────────────────────────────────────┘ │
│  [Clear Context] │                                                       │
│                  │  ─────────────────────────────────── separator ───  │
└──────────────────│  PROMPT-INPUT (textarea, auto-grow up to 5 rows)    │
                   │  [placeholder: "Message <project.name>…"]            │
                   │  [Cmd+K: command palette]          [Send ▶]          │
                   │  ──────────────────────────────────────────────────  │
                   │  STATUSLINE (1 row footer)                           │
                   │  provider (model) · ↑N ↓N · $X                      │
                   └─────────────────────────────────────────────────────┘
```

### 4.2 Per-Area Target State and Acceptance Criteria

#### prompt-input

**Target state**: `<textarea>` that auto-grows from 1 row up to 5 rows. `Enter` submits. `Shift+Enter` inserts newline. `Cmd+K` (Mac) / `Ctrl+K` (Windows/Linux) opens the command palette (see Section 4.3). Textarea is `disabled` while `isRunning`.

**Acceptance criteria**:
- Textarea auto-grows on newline input, max 5 rows before scrolling within the element.
- `Enter` submits; textarea cleared immediately on submit.
- `Shift+Enter` inserts literal newline; does not submit.
- `Cmd+K` / `Ctrl+K` opens the command palette overlay; does not insert `K` into textarea.
- Textarea `placeholder` is `Message <activeProject.name>…` (updates when active project changes).
- Send button is disabled when textarea is empty or `isRunning` is true.
- Textarea receives focus automatically when command palette is dismissed without selecting.

#### active-prompt

**Target state**: User bubble appears immediately on submit (before any backend response). Right-aligned, gray/surface background, rounded corners, timestamp `HH:MM`. No distinct "in-flight" styling — the bubble is permanent.

**Acceptance criteria**:
- User bubble appended to chat list within one render frame of submit.
- Bubble is right-aligned (`flex justify-end`).
- Timestamp reflects submit time in local timezone, format `HH:MM`.
- Bubble content is the literal submitted text (no markdown rendering for user messages).

#### active-response

**Target state**: An assistant placeholder bubble is created immediately on submit (empty, with `Loader2` spinner). As `task-progress` events arrive, the bubble content is replaced in place with the latest `message` payload (rendered as Markdown). On `task-complete`, final `narrative` replaces bubble content; spinner hidden; `isRunning = false`. On `task-error`, bubble renders in error state (red border, error text).

**Subagent-activity (inline)**: While `isRunning`, a collapsible activity strip renders inside the assistant bubble, above the bubble content. The strip shows the current delegation state sourced from SSE `Event` stream. Strip is collapsed by default after `SessionDone`; a `▶ Show activity` toggle reveals it in history.

**Acceptance criteria**:
- Placeholder bubble created before first `task-progress` event.
- `isRunning` spinner (`Loader2`) visible inside the placeholder bubble.
- Each `task-progress` payload replaces prior bubble content (not appended).
- Markdown rendering applied to all assistant bubble content.
- On `task-complete`: spinner removed, final content set, `isRunning = false`.
- On `task-error`: bubble border changes to destructive/red, error text shown.
- Subagent activity strip (within the bubble) updates on every relevant SSE event.
- After `SessionDone`, activity strip collapses; toggle `▶ Show activity (N steps)` allows re-expansion.
- Chat area auto-scrolls to bottom on each content update, unless user has scrolled up.

#### history

**Target state**: All messages for the active project, in chronological order. Timestamps per message (`HH:MM`). Auto-pins to bottom after each update. No hard client-side cap. Messages are project-scoped: switching active project switches the message list.

**Acceptance criteria**:
- `$activeMessages` keyed by `activeProjectId`; switching project replaces the visible list.
- Timestamps shown in local timezone.
- Auto-scroll to bottom after each update, except when user has scrolled up (detect via `scrollTop < scrollHeight - clientHeight - 50px`).
- When user has scrolled up and new message arrives: show "↓ New message" button pinned to bottom; clicking it scrolls to bottom.
- `user` role: right-aligned, gray bubble.
- `assistant` / `pm` role: left-aligned, teal/primary left-border, role badge (`pm` or `agent`).
- `system` / `status` role: centered, italic, dimmed.

#### subagent-activity

**Target state**: Inline within the active assistant bubble (see active-response above). Not a separate sidebar panel. Source: SSE events from the `/api/events` stream.

Activity strip content (per active session):
- Line 1: `✻ Delegating to <agent>… (Xs)` — from `PmDelegating` event
- Line 2: `↳ <runner_type> · <model>` — from `AgentStarted` + `LlmRequested`
- Line 3: latest step text — from `AgentMessage`, `PhaseStarted`, `ToolCalled`

**Acceptance criteria**:
- Activity strip visible in the assistant bubble while `isRunning`.
- Updates on every `PmDelegating`, `AgentSpawned`, `AgentStarted`, `AgentMessage`, `PhaseStarted`, `PhaseDone`, `PhaseSkipped`, `PersonaDetected`, `ToolCalled`, `ToolResult` event.
- Elapsed time in Line 1 increments every second (client-side timer started on `SessionStarted`).
- `PersonaDetected` prepends `[<persona>]` badge to Line 1.
- `PhaseSkipped` renders as `⊘ <phase> skipped` in Line 3.
- After session ends: strip collapses; `▶ Show activity (N steps)` toggle available.
- Expanding the toggle reveals all captured activity steps as a scrollable list within the bubble.

#### tool-calls

**Target state** (Phase 2): Tool invocations rendered as collapsible rows inside the active assistant bubble, below the activity strip. Each row: `▶ <tool-name>  <preview>`. Clicking expands to show full args (JSON, syntax highlighted) and result preview.

**Target state** (Phase 1 / interim): Not rendered. Tool activity appears only in the subagent-activity strip as step text.

**Acceptance criteria (Phase 2)**:
- Each `ToolCalled` event appends a collapsed row: `▶ <tool>  <preview truncated to 80 chars>`.
- Each `ToolResult` event updates the corresponding row: collapse icon changes to `✓`; result preview appended.
- Clicking a row expands/collapses it inline; expansion shows full JSON args and result in a `<pre>` block.
- Failed tool calls (where result preview indicates error) render with red `✗` icon.
- Tool call rows are delineated from activity strip and bubble content by a thin horizontal rule.

#### statusline

**Target state**: Single-row footer below the prompt-input area, spanning the full width of the main content column (not sidebar). Content: `<provider> (<model>) · ↑N ↓N · $X.XXX`

When no session has run yet: `Ready · <model>`. When an error occurred: `⚠ <error summary>` (red text).

**Acceptance criteria**:
- Footer row always visible below `InputArea`, above viewport bottom.
- Content updates within one render cycle of each `LlmResponded` event.
- Model and provider come from the most recent `LlmRequested` event.
- Tokens and cost are cumulative for the current session; reset to 0 on new session start.
- On narrow viewports (< 600px): show only `<model> · $X.XXX`; hide token counts.
- Font: monospace, small (`text-xs`), muted color (`text-muted-foreground`).

### 4.3 Command Palette

`Cmd+K` / `Ctrl+K` opens a centered modal overlay with a search input and list of commands. No slash-command textarea injection.

**Palette entries** (minimum set):

| Command | Action |
|---------|--------|
| Change model | Opens model picker (inline within palette or sub-overlay) |
| Change provider | Opens provider picker |
| Connect project | Opens project path input |
| Disconnect | Disconnects from current project |
| Clear context | Clears chat history for active project |
| New project | Opens setup wizard |
| Show tool calls | Expands all tool-call rows in current session bubble |

**Acceptance criteria**:
- Palette opens on `Cmd+K` / `Ctrl+K` when focus is anywhere in the window.
- Palette closes on `Esc`, on selection, or on click outside.
- Search input filters commands by substring match on command label.
- Keyboard: `Up` / `Down` navigate; `Enter` selects.
- Palette does not block background; uses translucent backdrop.

### 4.4 Projects Sidebar Spec

**Sidebar structure** (top to bottom):

1. **Header**: Logo mark + API health dot (green = healthy, red = error, amber pulse = starting/reconnecting).
2. **Aggregate dashboard row**: `N projects · N running · $X today` — updates every 5s via polling.
3. **Projects list**: All projects known to the harness (from registry scan of `.open-mpm/state/processes.json` and filesystem scan of `~/.open-mpm/` or locally configured paths). Each row:
   - Status dot (amber pulse = running, red = error, gray = idle)
   - Runner badge (`CC` for claude-code, `OR` for openrouter, `AN` for anthropic-direct)
   - Project name (truncated to fit, full name in tooltip)
   - Last-active timestamp (relative: "2m ago", "yesterday")
   - Active project: left-border accent + background tint
4. **Per-project actions** (on hover or right-click context menu):
   - Connect
   - Open in terminal (spawns terminal emulator at project path, platform-specific)
   - Clear context
   - Remove from list (does not delete files)
5. **"+ New Project" button**: Opens setup wizard (see below).
6. **Task History**: Scrollable list of recent tasks (existing `TaskHistory` component).
7. **Footer**: Theme toggle + Clear Context (global).

**Setup wizard** (triggered by "+ New Project"):
- Step 1: Path input with filesystem browser hint.
- Step 2: Auto-detect runner type from `.open-mpm/agents/pm.toml`; allow override.
- Step 3: Name input (defaults to directory basename).
- Wizard completes by registering the project in the harness registry and switching to it.

**Acceptance criteria**:
- Sidebar width: 288px fixed (`w-72`). Non-resizable in Phase 2.
- Project list scrolls independently of chat history.
- Status dots update in real-time via SSE `SessionStarted` / `SessionDone` events.
- Clicking a project row switches `activeProjectId`; triggers chat history swap.
- Right-click/hover actions are keyboard-accessible via focus + context menu key.
- Aggregate dashboard row counts: `running` = projects with active session; `$X today` = sum of session costs for current calendar day (UTC).
- Runner badges are tooltipped with full runner name.
- "Open in terminal" is disabled on platforms where no terminal emulator is detected.

---

## 5. Messaging (Telegram) Surface Spec (Surface C)

### 5.1 Per-Area Target State and Acceptance Criteria

#### prompt-input

**Target state**: Native Telegram compose bar. Slash commands parsed by teloxide: `/start`, `/help`, `/connect`, `/disconnect`, `/clear`, `/status`, `/tools`, `/projects`.

**Acceptance criteria**:
- All slash commands registered with `teloxide::commands_repl_with_listener` (or equivalent) so Telegram shows them in autocomplete.
- Free-text messages (non-slash) are forwarded as PM tasks.
- Messages over 4096 characters are rejected with a user-facing error: "Message too long (N chars). Telegram limit is 4096. Please split your request."

#### active-prompt

**Target state**: Not echoed by the bot. Telegram's native threading (`ReplyParameters`) associates the bot reply with the user message.

**Acceptance criteria**:
- Bot replies are threaded to the triggering user message via `ReplyParameters::new(msg_id)` on the last response chunk.
- No separate "received" acknowledgement message is sent.

#### active-response

**Target state**: `ChatAction::Typing` sent before LLM call (best-effort; re-sent every 4s if the call takes longer). Response sent as one or more HTML-formatted messages split at 4096-char boundaries (last newline before boundary). Final message threaded to user message.

**Acceptance criteria**:
- `ChatAction::Typing` sent within 500ms of receiving the user message.
- If LLM call exceeds 4s: re-send `ChatAction::Typing` (loop at 4s intervals until done).
- Response split at last `\n` before 4096-char boundary; continuation messages sent in order.
- HTML formatting via `markdown_to_html_safe()`: code blocks as `<pre><code>`, inline code as `<code>`, bold as `<b>`.
- On LLM error: send plain error message prefixed with `⚠ Error: `.

#### history

**Target state**: File-based session persistence with 24h TTL. On bot startup, sessions are loaded from `~/.open-mpm/state/telegram-sessions/<chat_id>.json`. On shutdown (or after each turn), sessions are saved. Sessions older than 24h are evicted on load. Max 20 turns per session (enforced in `run_pm_task_with_history`).

**Session file format**:
```json
{
  "chat_id": 123456789,
  "project_path": "/path/to/project",
  "turns": [...],
  "last_active_unix": 1746000000
}
```

**Acceptance criteria**:
- Session file written after each completed turn.
- On startup: sessions loaded; any with `last_active_unix` older than 86400s are discarded.
- `/clear` deletes the session file for the current chat and reinitializes an empty session.
- Sessions directory created on first run if absent.
- Session file writes are atomic (write to temp file + rename).
- Failure to write session file is logged as warning; bot continues to function.

#### subagent-activity

**Target state**: Not visible inline. Telegram "typing…" indicator is the only activity signal. No per-step output.

**Acceptance criteria**:
- No additional messages sent during agent delegation.
- `ChatAction::Typing` maintained throughout (re-sent every 4s).

#### tool-calls

**Target state**: Available on demand via `/tools` command. Not shown inline during response.

**`/tools` command**: Prints the last N tool calls for the current session as a structured HTML message. N = min(5, total calls). Format per tool call:
```
⤳ <tool-name>
   <preview of args, max 200 chars>
   → <preview of result, max 200 chars>
```

**Acceptance criteria**:
- `/tools` responds with up to 5 most recent tool calls from the current session.
- If no tool calls have occurred: "No tool calls in current session."
- Tool call data sourced from an in-memory `Vec<ToolCallRecord>` per session, populated from `ToolCalled` / `ToolResult` events.
- Response sent as `ParseMode::Html`.

#### statusline

**Target state**: No ambient statusline. `/status` command provides on-demand status.

**`/status` response format**:
```
Status
Project: <path or "not connected">
Model: <provider/model>
Turns: N / 20
Tool calls today: N
```

**Acceptance criteria**:
- `/status` responds within 1s (does not call the LLM).
- All fields sourced from in-memory session state.
- Response sent as `ParseMode::Html`.

### 5.2 Telegram Slash Command List

| Command | Description |
|---------|-------------|
| `/start` | Welcome message + command list |
| `/help` | List all commands with brief descriptions |
| `/connect <path>` | Connect to a project PM at the given path |
| `/disconnect` | Disconnect from current project |
| `/clear` | Clear conversation history and delete session file |
| `/status` | Show current model, project, turn count |
| `/tools` | Show last 5 tool calls in current session |
| `/projects` | List all known projects with status |
| `/connect <N>` | Connect to project by index from `/projects` list |

---

## 6. Projects UI Spec

### 6.1 TUI Projects View

**Trigger**: `/projects` slash command, or (Phase 3) `F2` key.

**Presentation**: Full-screen picker overlay using the same modal pattern as `/model` and `/provider` pickers, but full height. Title: `Projects`.

**Columns**:

| Column | Content | Width |
|--------|---------|-------|
| Name | Project name (basename of path) | 25% |
| Path | Absolute path, truncated from left | 35% |
| Status | `idle` / `running` / `error` | 10% |
| Last active | Relative time: "2m ago", "3h ago", "never" | 15% |
| Runner | `claude-code`, `openrouter`, `anthropic` | 15% |

**Navigation**:
- `Up` / `Down`: Move selection.
- `Enter`: Connect to selected project (equivalent to `/connect <path>`); dismiss overlay.
- `Esc`: Dismiss without action.
- `/` or typing: Filter rows by substring match on name or path.

**Data source**: `~/.open-mpm/state/processes.json` for running/error status; filesystem scan of configured project dirs for the full list. Data refreshed on overlay open (not live-polled while open).

**Status rendering**:
- `running`: yellow `●` glyph
- `error`: red `●` glyph
- `idle`: dim `○` glyph

**Acceptance criteria**:
- Overlay opens and populates within 200ms of `/projects` command.
- Filter input updates visible rows on each keystroke.
- Connecting to a project updates the scope label color (yellow) and statusline model.
- If no projects found: show "No projects found. Use /connect <path> to add one."

### 6.2 Web Projects View

The sidebar is the primary projects view (see Section 4.4). No separate full-screen view in Phase 2. The "+ New Project" button and per-project action menus are the primary management affordances.

### 6.3 Telegram Projects View

**`/projects` command response**:

```
Projects (N total, M running)

1. project-alpha  ● running  /Users/masa/projects/alpha
2. project-beta   ○ idle     /Users/masa/projects/beta
3. project-gamma  ✗ error    /Users/masa/projects/gamma

Use /connect <N> to switch projects.
```

**`/connect <N>` command**: Resolves index N from the last `/projects` response (stored in session state as `last_projects_list`). Connects to the matching project path.

**Acceptance criteria**:
- `/projects` lists all projects from the registry (same source as TUI and web).
- Status symbols: `●` running (bold), `○` idle, `✗` error.
- `/connect <N>` fails gracefully if N is out of range: "Invalid project number. Use /projects to see the list."
- Session state stores `last_projects_list: Vec<ProjectEntry>` to resolve `/connect <N>`.

---

## 7. Backend Event Model

The following table maps every `Event` variant to the rendering action on each surface. "—" means the event is ignored on that surface.

| Event | TUI Area | TUI Action | Web Area | Web Action | Telegram Action |
|-------|----------|------------|----------|------------|-----------------|
| `SessionStarted` | subagent-activity | Show panel; start timer; set `busy_since` | active-response | Create placeholder bubble; set `isRunning = true`; start client timer | Send `ChatAction::Typing`; start typing keep-alive loop |
| `SessionDone` | subagent-activity, history | Collapse panel; push final response to history | active-response, statusline | Replace bubble content with final narrative; stop spinner; update statusline tokens/cost | Send final response message; stop typing loop |
| `SessionCancelled` | subagent-activity, history | Collapse panel; push "Cancelled" status line | active-response | Show cancellation notice in bubble | Send "Request cancelled." |
| `PmThinking` | active-response | Append dim italic ThinkingStep line in history | active-response | Update activity strip Line 3 | — |
| `PmDelegating` | subagent-activity | Update Row 3 with delegation text | active-response | Update activity strip Line 1 with agent name + elapsed | — |
| `AgentSpawned` | subagent-activity | Update Row 2 with agent name | active-response | Update activity strip Line 2 | — |
| `AgentStarted` | subagent-activity | Update Row 2 with `runner_type` | active-response | Update activity strip Line 2 with runner badge | — |
| `AgentMessage` | active-response | Append ThinkingStep line in history | active-response | Update activity strip Line 3 | — |
| `AgentDone` | subagent-activity | Append "✓ <agent> done" ThinkingStep | active-response | Append step to activity strip history | — |
| `AgentFailed` | history | Append Error line: "✗ <agent>: <error>" | active-response | Show error in bubble; red border | Included in final error response |
| `ToolCalled` | history, subagent-activity | Append "⤳ <tool>: <preview>" Status line; update Row 3 | tool-calls | Append collapsed tool-call row in bubble | Store in `ToolCallRecord` for `/tools` |
| `ToolResult` | history | Append "✓ <tool>: <preview>" Status line | tool-calls | Update tool-call row with result; change icon to `✓` or `✗` | Store in `ToolCallRecord` for `/tools` |
| `PhaseStarted` | subagent-activity | Update Row 3 with phase name | active-response | Update activity strip Line 3 | — |
| `PhaseDone` | subagent-activity | Append "✓ <phase>" ThinkingStep | active-response | Append step to activity strip history | — |
| `PhaseSkipped` | subagent-activity | Row 3: "⊘ <phase> skipped (<persona>)" | active-response | Activity strip: "⊘ <phase> skipped" | — |
| `PersonaDetected` | subagent-activity | Prepend `[<persona>]` to Row 3 | active-response | Activity strip Line 1: badge `[<persona>]` | — |
| `LlmRequested` | statusline | Update provider + model; note in-flight | statusline | Update statusline model | — |
| `LlmResponded` | statusline, subagent-activity | Update tokens + cost in statusline; update Row 1 token counters | statusline | Update footer token + cost counters | — |
| `ReportGenerated` | history | Append Status line: "<agent> returned N words" | active-response | Activity strip step: "<agent> · N words" | — |
| `Ping` | — | No-op (keepalive) | — | No-op | — |

---

## 8. Consistency Matrix

`✅` = Implemented and meets spec  
`⚠` = Partial implementation (gap described)  
`❌` = Not implemented  
`—` = Not applicable to this surface

| Area | TUI | Web | Telegram |
|------|-----|-----|----------|
| prompt-input (basic) | ✅ | ✅ | ✅ |
| prompt-input (slash commands) | ✅ | ❌ (Cmd+K palette — Phase 2) | ✅ |
| prompt-input (history recall) | ✅ | ❌ (deferred) | — |
| active-prompt | ✅ | ✅ | — |
| active-response (per-event) | ✅ | ⚠ (per task-progress; no SSE event fan-out) | ✅ (single-shot) |
| active-response (streaming) | ❌ (pending SSE — Phase 4) | ❌ (pending SSE — Phase 4) | — |
| history (display) | ✅ | ✅ | — (native Telegram) |
| history (timestamps) | ❌ (no timestamps in TUI) | ✅ | — |
| history (persistence) | ✅ (repl_history.txt) | ❌ (in-memory per session) | ❌ (Phase 3: file persistence) |
| subagent-activity (panel) | ✅ | ❌ (Phase 2: inline bubble) | — |
| subagent-activity (agent names) | ✅ | ❌ | — |
| subagent-activity (elapsed time) | ✅ | ❌ | — |
| tool-calls (text display) | ⚠ (tool name only) | ❌ | — |
| tool-calls (structured cards) | ❌ (Phase 4) | ❌ (Phase 2) | — |
| tool-calls (on-demand /tools) | ❌ (Phase 1) | — | ❌ (Phase 3) |
| statusline (model/provider) | ✅ | ❌ (Phase 2: footer) | — |
| statusline (tokens/cost) | ✅ | ❌ (Phase 2: footer) | — |
| statusline (on-demand /status) | ✅ | — | ✅ |
| projects (list) | ⚠ (text-only `/projects`) | ✅ (sidebar) | ❌ (Phase 3) |
| projects (visual picker) | ❌ (Phase 1) | ✅ (sidebar buttons) | — |
| projects (per-project actions) | ⚠ (connect only) | ❌ (Phase 2) | ⚠ (connect only) |
| projects (dashboard row) | — | ❌ (Phase 2) | — |
| session persistence | ✅ (repl_history) | ❌ | ❌ (Phase 3) |

---

## 9. Design Constraints

### Terminal (TUI)

- **Minimum width**: 80 columns. Layouts must degrade to 80 cols without panic or overlap.
- **Minimum height**: 24 rows. Banner collapses automatically below 20 rows.
- **Colors**: Use ratatui named colors (`Color::Green`, `Color::Yellow`, etc.) that map to terminal theme colors. Do not hardcode 256-color or RGB values in default config; allow config override.
- **Cursor**: Always rendered via `set_cursor_position`; never via ANSI escape sequences directly.
- **No mouse dependency**: All interactions must be keyboard-accessible.
- **Unicode glyphs**: `✻`, `❯`, `⏺`, `✗`, `⤳`, `✓`, `⊘`, `↳` must render correctly in UTF-8 terminals. Fallback ASCII equivalents (`*`, `>`, `o`, `X`, `>`, `+`, `~`, `\\`) configurable via `ascii_mode = true` in config.

### Web/GUI (Tauri + Svelte)

- **Tauri IPC latency**: Assume 5–20ms round-trip for Tauri commands. Do not assume synchronous IPC.
- **No true token streaming**: Until the backend adds SSE, content updates are per `task-progress` event (typically 1–3 events per session). UI must not assume token-level updates.
- **SSE connection**: The web UI connects to `http://localhost:<port>/api/events` for real-time events. If SSE disconnects: show amber health dot; attempt reconnect every 5s; fall back to polling `/api/health` every 10s.
- **Responsive**: Minimum supported viewport: 768px wide. Sidebar may collapse below 900px (hamburger menu pattern — deferred to Phase 3).
- **Accessibility**: Status dots must have `aria-label` attributes. Color must not be the sole information carrier (pair with icon or text).

### Messaging (Telegram)

- **Message length limit**: 4096 characters. All outgoing messages must be split before this boundary.
- **Parse mode**: `Html` for all structured responses. Plain text fallback if HTML send fails.
- **Statelessness**: Bot must handle restart gracefully. Any state that must survive restart goes to the session file.
- **No streaming**: Telegram does not support message streaming. All responses are single-shot.
- **Rate limits**: Telegram Bot API allows 30 messages/second globally, 1 message/second per chat. Bot must not send multi-part responses faster than 1/second per chat.

---

## 10. Phased Implementation Plan

### Phase 1 — TUI hardening (immediate)

**Scope**: TUI surface only. No new surfaces or backend changes.

| Item | Area | Effort |
|------|------|--------|
| Projects full-screen picker (`/projects`) | projects | Medium |
| Tool-call text display (`⤳ tool: preview`) | tool-calls | Small |
| Tool-call result display (`✓ tool: result`) | tool-calls | Small |
| `/tools` slash command (recent tool calls) | tool-calls | Small |
| Timestamp on history entries | history | Small |
| "N new messages" indicator when scrolled up | history | Small |
| ASCII fallback mode for non-UTF-8 terminals | prompt-input | Small |

**Exit criteria**: All Phase 1 items in the consistency matrix column TUI are `✅`.

### Phase 2 — Web surface parity

**Scope**: Web/GUI. Requires SSE event consumer in frontend and backend `/api/events` endpoint.

| Item | Area | Effort |
|------|------|--------|
| SSE event consumer (connect to `/api/events`) | infrastructure | Medium |
| Subagent activity strip (inline in bubble) | subagent-activity | Medium |
| Footer statusline (model, tokens, cost) | statusline | Small |
| Cmd+K command palette | prompt-input | Medium |
| Per-project sidebar actions (connect, clear, remove) | projects | Small |
| Aggregate dashboard row in sidebar | projects | Small |
| "+ New Project" setup wizard | projects | Medium |
| Structured tool-call rows (Phase 2 version: text, collapsible) | tool-calls | Medium |
| Session history persistence (localStorage per project) | history | Small |

**Exit criteria**: All Phase 2 items in the Web column are `✅` or `⚠` (partial acceptable for wizard).

### Phase 3 — Telegram parity

**Scope**: Telegram surface.

| Item | Area | Effort |
|------|------|--------|
| File-based session persistence (24h TTL) | history | Medium |
| `/tools` command (last 5 tool calls) | tool-calls | Small |
| `/projects` command + `/connect <N>` | projects | Small |
| `ChatAction::Typing` keep-alive loop | active-response | Small |

**Exit criteria**: All Phase 3 items in the Telegram column are `✅`.

### Phase 4 — Structured tool cards and token streaming (all surfaces)

**Scope**: All surfaces. Requires backend SSE token streaming (significant backend work).

| Item | Area | Effort |
|------|------|--------|
| Backend: SSE token streaming endpoint | infrastructure | Large |
| TUI: collapsible tool-call cards in history | tool-calls | Medium |
| Web: collapsible tool-call cards in bubble | tool-calls | Medium |
| TUI: token-level streaming in active-response | active-response | Medium |
| Web: token-level streaming in active-response | active-response | Medium |

**Exit criteria**: All surfaces show token-level streaming. Tool-call cards implemented on TUI and Web.

---

*Document status: canonical. Update this file when any UI behavior changes. Reference from implementation PRs.*
