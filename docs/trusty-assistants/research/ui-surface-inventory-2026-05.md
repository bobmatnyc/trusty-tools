# UI Surface Inventory — open-mpm

**Date**: 2026-05-02
**Purpose**: Pre-spec research — gather current state of all UI surfaces to inform a
multi-surface UI spec.

---

## 1. Surface Inventory

### Surface A: TUI (ratatui / terminal)

Files: `src/repl/tui.rs`, `src/repl/mod.rs`

| Area | Current implementation |
|---|---|
| **Prompt input** | Single-line editor (`input_buf` + `cursor_pos`). Rendered borderless at a fixed `Constraint::Length(1)` row. Prefix: colored `❯` glyph (cyan for user-scope, yellow for project-scope). Cursor drawn with `set_cursor_position`. Ctrl-A/E (home/end), Ctrl-U (clear), Ctrl-C (cancel input), Ctrl-D (quit on empty). |
| **Active prompt** | After Enter, the user line is immediately echoed to the chat scrollback with a `❯` prefix (green). The input row is cleared. While the LLM is busy, a `[thinking…]` hint appears inline below the last user entry in the chat area (not in the input row). |
| **Active response** | Not truly streamed yet. `ThinkingStep` events provide curated status lines ("Delegating to engineer…", "engineer · running…") in the chat below the last user line (dim, italic). The `streaming_preview` field shows the latest step in a dedicated 1-row preview area above the input. When `LlmResponse` arrives the full response is pushed to chat as an `Assistant` or `Error` entry. |
| **History** | Chat scrollback: `Vec<ChatLine>` with `ChatRole` (User / Assistant / Error / Status). Scrollable with PageUp/PageDown (10-line steps). `show_banner` flips false as soon as any chat entry exists. REPL input history (up-arrow recall): `Vec<String>` in memory + persisted to `~/.open-mpm/repl_history.txt`. Up-arrow recalls `last_prompt`; full history navigable with up/down after that. |
| **Subagent activity** | 3-row activity panel appears only while busy (`thinking` or `busy_since` is set). Row 1: spinner glyph `✻` (yellow) + "Processing… (Xs · ↑N ↓N tokens · thinking/working/processing)". Row 2: `↳ <model_name>` (dim italic). Row 3: latest `ThinkingStep` text (dim italic). Panel collapses when idle. |
| **Tool calls** | Not rendered as distinct UI elements. Tool invocations surface only as `ThinkingStep` text: "Calling tool: <name>…" emitted by `spawn_thinking_relay` when `Event::ToolCalled` fires. No structured tool-call display. |
| **Statusline** | Single row at the bottom. Rich format: `[open-mpm]` (cyan bold) + `✓ LLM: <provider> (<model>)` (green tick + bold model) + ` · ↑N ↓N · $0.xxx · All systems go.` (token/cost injected after accumulation). Configured via `StatuslineConfig` from `.open-mpm/config.toml`; segments include `workdir`, `git_branch`, `git_dirty`, `model`, `provider`, `elapsed`. Provider picker and model picker surface as centered modal overlays (50%×60%, cyan border). |

**Notable TUI features not in the 7-area list:**
- Banner panel (12-row, collapsed once chat starts): two-column with logo + user/project label (left), recent git commits + command hints (right).
- Picker overlay: modal list for `/model` and `/provider` with Up/Down/Enter/Esc navigation.
- `/projects` command: lists known projects from the registry; `/connect <path>` switches project and updates scope (User vs Project label color).
- Agent scope: User scope = cyan prompt label; Project scope = yellow prompt label.

---

### Surface B: Web/GUI (Tauri + Svelte)

Files: `ui/src/components/ChatView.svelte`, `ui/src/components/InputArea.svelte`,
`ui/src/components/Sidebar.svelte`, `ui/src-tauri/src/main.rs`

| Area | Current implementation |
|---|---|
| **Prompt input** | `InputArea.svelte`: `<textarea rows="2">` with `placeholder="Message <project.name>…"`. Enter to submit (Shift+Enter for newline). Disabled while `$isRunning`. Send button with lucide `Send` icon. No slash-command support exposed in the web UI input. |
| **Active prompt** | User bubble appears immediately on submit (right-aligned, rounded, light gray/dark surface). No "in-flight" distinction — the user bubble is identical before and after response. |
| **Active response** | Assistant placeholder bubble created immediately with empty content. Three Tauri event listeners (`task-progress`, `task-complete`, `task-error`) mutate the placeholder in place via `updateMessageByTask`. Progress events stream content into the growing bubble. On `task-complete`, final `narrative` replaces bubble content. `isRunning` shows a `Loader2` spinner + "Running…" row at bottom of chat list. No streaming text within the bubble — entire `message` payload replaces prior content on each progress event. |
| **History** | `$activeMessages` store: `Vec` of message objects per project, keyed by `activeProjectId`. `ChatView.svelte` renders all messages for the active project. Scroll auto-pinned to bottom after each update (`afterUpdate` → `scrollEl.scrollTop = scrollEl.scrollHeight`). Timestamps shown as `HH:MM`. No pagination yet. |
| **Subagent activity** | Not separately rendered. Only the `isRunning` spinner and growing assistant bubble content indicate in-flight sub-agent work. No dedicated activity strip. |
| **Tool calls** | Not rendered. Tool calls are not surfaced in the web UI at all. |
| **Statusline** | None. The Sidebar header shows an API-health dot (green = API ready, red = error, amber spinner = starting). No model/provider/token info visible anywhere in the web UI. |

**Message roles in ChatView:**
- `user` — right-aligned, gray bubble.
- `assistant` — left-aligned with teal left-border, "agent" label.
- `pm` — left-aligned with primary-color left-border, "pm" label.
- Other — centered italic (system/status messages).

**Sidebar (`Sidebar.svelte`):**
- Fixed 288px (`w-72`) left column.
- Header: logo + API health dot.
- Nav section "Projects": lists `$projects` store items. Each project is a button with a status dot (amber pulse = running, red = error, gray = idle), a `Folder` or `Terminal` icon (ctrl uses Terminal), and truncated name. Active project has left-border highlight + background tint.
- Below nav: `TaskHistory` component (scrollable).
- Footer: `ThemeToggle` + "Clear Context" button (POSTs `/api/clear-context` + reloads).

**Tauri backend (`ui/src-tauri/src/main.rs`):**
- Spawns `open-mpm --api --port <port>` sidecar on first window open.
- Commands: `ensure_api_server`, `send_message`, `list_tasks`, `check_health`.
- `send_message` polls the REST task endpoint and emits `task-progress` / `task-complete` / `task-error` Tauri events into the frontend.
- No Tauri-specific layout logic beyond command registration; layout is entirely in Svelte.

---

### Surface C: Messaging / Telegram

File: `src/telegram/mod.rs`

| Area | Current implementation |
|---|---|
| **Prompt input** | Free-text Telegram messages are the input. Slash commands: `/start`, `/help`, `/connect <path>`, `/clear`, `/status`. No prompt box — Telegram native compose bar. |
| **Active prompt** | Not echoed. Telegram's own reply threading shows the user's message; the bot uses `ReplyParameters::new(user_msg_id)` to thread its response to the user's message (last chunk only). |
| **Active response** | `ChatAction::Typing` indicator sent before LLM call (best-effort). Response is a blocking single call to `ctrl::run_pm_task_with_history`; no streaming — the bot awaits the full response then sends. Long responses split at 4096-char boundaries (last newline before boundary). Sent as `ParseMode::Html`. |
| **History** | Per-chat `ChatSession` with `history: Vec<ConversationTurn>`. In-memory only; no persistence. `/clear` wipes the history for the active chat. Max 20 turns (inherits `MAX_HISTORY_TURNS` via `run_pm_task_with_history`). |
| **Subagent activity** | Not visible to the user. Only the Telegram "typing…" indicator signals activity. |
| **Tool calls** | Not visible. Tool calls happen inside `run_pm_task_with_history` and are not surfaced in Telegram. |
| **Statusline** | None. `/status` command shows project path, history turn count, and LLM label as a structured HTML message. |

**Markdown-to-HTML conversion:**
Ctrl responses arrive as Markdown. `markdown_to_html_safe()` converts: strips ANSI, HTML-escapes, converts ` ```lang … ``` ` to `<pre><code>`, inline `` `code` `` to `<code>`, `**bold**` to `<b>`. Fallback to plain text on send failure.

---

## 2. Gap Analysis

### Prompt input
- TUI: full single-line editor with cursor, history recall, slash commands.
- Web: `<textarea rows="2">`, no slash commands, no history recall.
- Telegram: native Telegram compose; slash commands handled via teloxide parser, not repl-style.
- **Gap**: Web has no slash command support; no history navigation. Telegram has no freeform cancel/clear inline — only `/clear` command.

### Active prompt (in-flight visibility)
- TUI: user line echoed immediately to scrollback; thinking-step lines appear below it.
- Web: user bubble appears immediately; assistant placeholder visible immediately.
- Telegram: nothing shown until response arrives (only "typing…" indicator from Telegram platform).
- **Gap**: Telegram has no per-step progress. Web has no thinking-step text — only a spinner icon.

### Active response (streaming)
- TUI: not truly streamed — `ThinkingStep` events serve as proxy. Final response lands as full text.
- Web: per-event mutation of bubble content via `task-progress` events — closest to streaming. No word/token-level streaming.
- Telegram: single-shot; no streaming.
- **Gap**: None of the surfaces has true token-level streaming. TUI has the most visible activity (3-row activity panel + preview row). Web has the least structured progress. Telegram has none.

### History (conversation scrollback)
- TUI: scrollable, keyed by `ChatRole`. No timestamps. No pagination.
- Web: scrollable, auto-pins to bottom, timestamps per message. All messages for active project shown. No pagination.
- Telegram: native Telegram history (unlimited client-side); bot's server-side history is in-memory only, max 20 turns.
- **Gap**: TUI has no timestamps. Telegram has no server-side persistence. Web has no search or history cap display.

### Subagent activity
- TUI: dedicated 3-row activity strip while busy. Named agent steps visible.
- Web: only spinner + growing bubble. No agent names or step labels.
- Telegram: completely invisible.
- **Gap**: Web and Telegram lack subagent activity visibility.

### Tool calls
- TUI: surfaced as `ThinkingStep` text only ("Calling tool: X…").
- Web: not surfaced.
- Telegram: not surfaced.
- **Gap**: No surface renders tool calls as structured UI elements (with input/output). TUI shows tool name only.

### Statusline
- TUI: rich bottom bar — provider, model, tokens, cost, git branch, workdir.
- Web: API health dot only (sidebar header).
- Telegram: `/status` command only — not ambient.
- **Gap**: Web and Telegram lack ambient model/token/cost visibility.

### Project navigation
- TUI: `/projects` (text list) + `/connect <path>` to switch. Scope displayed as label color (cyan=user, yellow=project).
- Web: Sidebar project list — clickable buttons, status dots, icons. Active project highlighted.
- Telegram: `/connect <path>` only — no list command; per-chat project path.
- **Gap**: TUI has no visual project list (only text output of `/projects`). Telegram has no way to list or discover available projects.

---

## 3. ai-commander Projects Nav Pattern

Source: `docs/research/ai-commander-tmux-client.md`, `docs/research/tauri-chat-interface-design.md`

### ai-commander TUI
- `ViewMode` enum: `Normal`, `Inspect`, `Sessions`.
- Sessions view (`tui/sessions.rs`): lists tmux sessions with project name → tmux session mapping.
- `App.sessions: HashMap<String, String>` maps project name to session name.
- Toggled with a key (F2 cycles modes).
- Session items shown with project name; connecting binds the active pane to that session.

### ai-commander GUI (`commander-gui` Tauri)
- `SessionList.svelte` is the left sidebar (equivalent to open-mpm's `Sidebar.svelte`).
- Shows list of active sessions (not just projects) with connect/disconnect/stop actions.
- Each session item shows: session name, adapter type, status indicator.
- `DashboardView.svelte` shows aggregated stats across sessions.
- `ChatView.svelte` shows messages for the selected session.

### What "projects nav" looks like in ai-commander GUI
- Left sidebar lists all known sessions.
- Clicking a session makes it the active target for chat.
- Status dot per session: indicates running/idle/error.
- No separate "project" concept from session — each session is a project instance.

### How this maps to open-mpm's web Sidebar
Open-mpm's `Sidebar.svelte` mirrors this pattern well:
- `$projects` store ≈ ai-commander's session list.
- Status dot (amber pulse / red / gray) ≈ ai-commander's status indicators.
- `ctrl` project gets a `Terminal` icon; others get `Folder`.
- Active project highlighted with left-border + background tint.

### What's missing vs. ai-commander
- ai-commander GUI has connect/disconnect/stop actions per session from the sidebar.
- open-mpm Sidebar has no such per-project actions — only global "Clear Context".
- ai-commander shows adapter type (ClaudeCode, MPM, etc.) per session.
- open-mpm shows no agent/provider info in the sidebar.
- ai-commander has a `DashboardView` for aggregate stats.
- open-mpm has no dashboard view.

---

## 4. Recommended Spec Structure

A formal multi-surface UI spec for open-mpm should cover:

1. **Surface definitions** — authoritative list of surfaces (TUI, Web/GUI, Telegram, future: API-only / headless). Each surface's primary use case and user persona.

2. **Universal interaction model** — the 7 areas (prompt input, active prompt, active response, history, subagent activity, tool calls, statusline) defined abstractly, independent of surface. Canonical vocabulary.

3. **Per-surface implementation spec** — for each of the 7 areas, per surface:
   - Current state (from this inventory).
   - Target state (what it should do).
   - Acceptance criteria.
   - Out of scope / deferred.

4. **Event model** — how backend events map to UI updates on each surface:
   - `Event::PmDelegating` → subagent activity
   - `Event::ToolCalled` → tool call display
   - `Event::LlmRequested` / `Event::LlmResponded` → token counters
   - `task-progress` / `task-complete` (Tauri) → response streaming
   - Telegram `ChatAction::Typing` → busy indicator

5. **Project navigation spec** — authoritative definition of the project model:
   - What is a "project" vs a "session" vs a "scope"?
   - How do users discover, switch, and manage projects on each surface?
   - Registry format and persistence contract.

6. **Statusline / ambient info spec** — what info is always visible, what is on-demand, what is per-turn. Token display, cost, model, provider, git context.

7. **Tool call display spec** — how tool invocations are shown (collapsed/expanded, input args, output, error state). Currently absent on all surfaces.

8. **Consistency matrix** — table of surface × area × status (implemented / partial / missing / out-of-scope).

9. **Design constraints** — terminal width assumptions, Telegram message limits (4096 chars), Tauri event latency, accessible color choices for TUI (avoid 256-color assumptions).

10. **Open questions** (see section 5 below).

---

## 5. Key Design Decisions to Resolve

1. **Does subagent activity go in sidebar or inline?**
   - TUI puts it inline (activity strip, collapsed when idle).
   - Web could put it in a sidebar panel OR inline in the chat bubble.
   - Sidebar approach: persistent, always visible — good for long-running tasks.
   - Inline approach: associated with the turn that triggered it — better for context.
   - Decision needed: is subagent activity a per-turn artifact or a global session state?

2. **Does the web UI get slash commands?**
   - Currently: none in `InputArea.svelte`.
   - TUI has a rich slash command set (`/connect`, `/model`, `/provider`, `/agent`, etc.).
   - Implementing slash commands in a textarea with autocomplete is significant work.
   - Alternative: expose key actions as sidebar buttons or a command palette (Cmd+K).

3. **Is streaming per-token or per-step?**
   - TUI: per-`ThinkingStep` event (agent lifecycle events, not tokens).
   - Web: per-`task-progress` event (content chunks from the backend).
   - True token streaming would require SSE or WebSocket from the REST server.
   - Spec should commit to one model before implementation.

4. **Project scope vs. agent scope — which concept is surfaced to the user?**
   - TUI exposes `AgentScope` (User=ctrl, Project=connected project) as label color.
   - Web exposes projects as a list (each project can have a pm agent).
   - These are different mental models. Spec needs to reconcile them.

5. **Telegram history persistence — session or file?**
   - ai-commander persists sessions to JSON with 24h TTL.
   - open-mpm uses in-memory only; bot restart wipes all chat history.
   - Decision: add file persistence with TTL, or accept stateless restart?

6. **Tool call display — structured or text only?**
   - Current: tool name in a `ThinkingStep` string on TUI; nothing on web/Telegram.
   - Spec option A: text-only (current TUI approach — extend to all surfaces).
   - Spec option B: structured collapsible cards (tool name, args preview, output snippet).
   - Option B requires significant new event infrastructure.

7. **Web statusline — add one or not?**
   - Currently none. Token/cost/model info is dark.
   - Minimal option: add a footer bar below `InputArea` with model + token count.
   - Full option: match TUI statusline richness.
   - Telegram equivalent: always-on status would be intrusive; `/status` command is appropriate.

8. **Banner / welcome screen scope**
   - TUI has a 12-row banner (collapsed after first chat entry).
   - Web has an empty-state placeholder ("Start chatting. Messages will appear here.").
   - Should the web have a richer welcome state showing git commits, agent list, skills?

---

*Research captured: `docs/research/ui-surface-inventory-2026-05.md`*
