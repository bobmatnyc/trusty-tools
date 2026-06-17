# DOC-13 — Coordinator TUI (`tm coordinator`)

**Status:** Draft
**Subsystem:** trusty-mpm — TUI / coordinator
**Owner:** Engineering (trusty-mpm)
**Last-updated:** 2026-06-15
**Spec ID:** `SPEC-TUI-COORD-01~draft` (DOC-13)
**Prior art:** `open-mpm` REPL TUI (`/Users/masa/Projects/open-mpm/src/repl/`, `src/tm/`)
**Builds on:** existing `trusty-mpm/src/tui/` coordinator dashboard (`tm tui`)
**Cross-ref:** session-manager managed API (`crates/trusty-mpm/src/daemon/managed_routes/`),
coordinator endpoints (`crates/trusty-mpm/src/daemon/coordinator.rs`,
`.../api/coordinator_routes.rs`), the `session-manager-driver` skill, and issues
**#1268** (daemon URL vs bind port), **#1269** (trust-dialog / headless-spawn blocker).

> **Scope note.** This is a **behavior + UX requirements** spec. It does **not**
> implement the TUI. It specifies what the coordinator TUI must do, how it should
> look, which existing/new daemon APIs it consumes, and which `open-mpm` modules
> are portable. The PR that carries this doc opens **no** Rust changes.

---

## 1. Purpose & Goals

### 1.1 What we are building

A built-in **coordinator TUI** for `trusty-mpm` that resembles Claude Code: a
single text **input box** at the bottom of the screen, and **below the input** a
live **list of active sessions**. The operator types into the input to talk to a
**coordinator** (a PM-like agent) that can answer cross-session questions and
**create + manage** sessions. The session list shows one bullet for the
**controller** (the coordinator itself) and one bullet per managed session; the
**active** session row renders **two columns**: `[session ID] | [last summarized
message]`.

This is the conversational front door to the fleet of durable, tmux-backed
Claude Code sessions that the session manager already owns (see the
`session-manager-driver` skill). It replaces the need to drive the fleet through
discrete `tm session …` CLI calls for the common "talk to my fleet, spin up
work, watch it progress" loop.

### 1.2 Goals

- **G1 — One conversational surface.** A persistent input box (Claude-Code-like)
  that routes free text to a coordinator agent and routed commands (`@prefix:`)
  to a named session.
- **G2 — Live fleet visibility.** A session list under the input that refreshes
  live, showing the controller bullet + one bullet per managed session, with the
  active row in two columns: **ID | last summarized message**.
- **G3 — In-TUI session management.** Slash-command modals to create and manage
  sessions (`/new`, `/sessions`, `/attach`, `/stop`, `/resume`, `/kill`,
  `/help`) without leaving the TUI.
- **G4 — Reuse, don't reinvent.** Port the proven `open-mpm` ratatui REPL
  scaffolding (event loop, handler trait, picker/modal pattern, terminal RAII)
  rather than writing a new TUI framework, and evolve the **existing**
  `trusty-mpm` coordinator dashboard rather than starting from scratch.
- **G5 — Works without an LLM key.** Core fleet visibility and management must
  function with `OPENROUTER_API_KEY` **absent**; LLM features (coordinator
  free-text answers, LLM-quality session summaries) degrade gracefully.

### 1.3 Non-goals (see §10)

Editing code inside a session, a graphical/web UI (that is `trusty-console` /
`trusty-mpm-gui`), and replacing the `tm` CLI.

---

## 2. Background: what already exists

A near-complete coordinator dashboard already ships under
`crates/trusty-mpm/src/tui/` behind the `tui` feature (`ratatui` 0.29 + `crossterm` 0.28,
shared via `[workspace.dependencies]`), launched by `tm tui [--url <U>] [--interval-ms <N>]`
(`src/bin/tm/main.rs` → `trusty_mpm::tui::run(resolved, interval_ms)`).

Today it provides:

| Capability | Where | Notes |
|---|---|---|
| Coordinator chat pane + `CMD>` input bar (history ring) | `tui/dashboard/mod.rs` (`CommandBar`, `DashboardState`, `render`) | Input is **at the bottom**, chat above it, session **sidebar** to the side. |
| Session **sidebar** | `tui/dashboard/mod.rs` (`SessionRow`) | A side panel, not the "below the input" list this spec asks for. |
| Poll `GET /api/v1/sessions/context` → session rows | `tui/mod.rs::poll_daemon` + `tui/client.rs` | Timer-based, `--interval-ms` (default poll). |
| Send to `POST /api/v1/sessions/chat` | `tui/mod.rs::coordinator_send` | Free text → LLM; `@prefix:` → routed command. |
| Health screen (`[2]`), screen switching | `tui/mod.rs` (`Screen`), `tui/health/` | Secondary surface. |
| Self-healing daemon URL re-resolution | `tui/mod.rs::rediscover_daemon` + `core/discovery.rs` | Re-reads `~/.trusty-mpm/daemon.lock` on failed poll. |

**Conclusion:** the coordinator TUI is an **evolution** of `tui/`, not a new
binary from zero. The two structural gaps versus this spec are: (a) the session
list must move **below the input** (controller bullet + per-session bullets,
active row in two columns), and (b) the **slash-command modals** for full session
lifecycle management (`/new`, `/attach`, `/stop`, `/resume`, `/kill`) do not yet
exist — today only `@prefix:` routed commands and free-text chat exist.

---

## 3. UX & Layout

### 3.1 Screen layout (ASCII mockup)

The coordinator TUI is a single full-screen view. **Input box on top of the
session list**, session list fills the rest, status bar pinned at the very
bottom. (Claude Code keeps its composer at the bottom of a scrollback; here the
input sits directly **above** the live session list per the brief — "a text
input box; BELOW the input, a live list of active sessions".)

```
┌─ trusty-mpm coordinator ───────────────────────── daemon ●  http://127.0.0.1:7880 ┐
│ coordinator › spin up a session on aipowerranking for ticket #412 and watch it_   │   ← input box (focused)
├───────────────────────────────────────────────────────────────────────────────────┤
│ SESSIONS (3)                                              [↑↓] select  [↵] focus    │
│                                                                                     │
│ ● controller          coordinator        idle · 0 delegations · ready              │   ← the controller bullet
│ ● 4f9c…a1   ▸ aipowerranking  │ Running tests — 12 passed, fixing flaky timeout    │   ← ACTIVE row: [ID] | [summary]
│ ○ 7b2e…c0     genealogy       │ Awaiting approval: write to .github/workflows/…    │
│ ○ d1a8…ff     smarterthings   │ Idle — last activity 6m ago                        │
│                                                                                     │
│                                                                                     │
├───────────────────────────────────────────────────────────────────────────────────┤
│ /new /sessions /attach /stop /resume /kill /help   ·   ↵ send   ·   ? help   q quit │   ← status / key bar
└───────────────────────────────────────────────────────────────────────────────────┘
```

Layout regions (ratatui vertical `Layout`):

1. **Input box** (top, `Constraint::Length(3)`): a single-line editable
   composer with a `coordinator ›` prompt, cursor, and a ↑/↓ history recall ring
   (reuse `CommandBar`). Submitting (`Enter`) routes per §5.
2. **Session list** (`Constraint::Min(…)`): the live list. **Row 1 is always the
   controller bullet.** Rows 2..n are managed sessions, newest/most-active first.
   The **selected** ("active") row renders two columns separated by a `│` glyph:
   left = short session ID (first 8 hex of the UUID), right = the **last
   summarized message** (§6). Non-selected rows show ID + prefix + a dimmed
   one-line status. (See §3.3 for the column contract.)
3. **Status / key bar** (bottom, `Constraint::Length(1)`): slash-command hints +
   global keys, reversed style.

When a **slash-command modal** is open it renders as a centered overlay
(`Clear` + bordered block) on top of this view (port `draw_picker` +
`centered_rect`, §4.2).

### 3.2 Selection, highlight, scrolling, refresh

- **Selection / highlight.** Exactly one row is "active" (selected). The
  controller bullet is selectable. Up/Down (`↑`/`↓`, and `k`/`j`) move the
  selection; the selected row is bold/cyan and is the only row that expands into
  the two-column `ID | summary` view. Selection survives a refresh by session ID
  (clamp if the session disappears) — mirror `DashboardState::clamp_selection`.
- **Bullets.** `●` = running/active session (and the controller); `○` = idle/
  paused/stopped; a status-colored glyph may encode `AwaitingApproval` (e.g.
  yellow `◍`). The controller bullet is always row 1 and always `●`.
- **Scrolling.** When sessions exceed the visible region, the list scrolls with
  the selection (ratatui `ListState` offset); `PageUp`/`PageDown` jump a page;
  mouse wheel scrolls (port the `EnableMouseCapture` + `Scroll(±n)` event from
  `open-mpm` `tui.rs`).
- **Live refresh.** The list refreshes on a timer (`--interval-ms`, default
  ~1500 ms) AND immediately after any action that mutates the fleet (after a
  `/new`, `/stop`, etc., re-poll once so the UI reflects the change without
  waiting for the next tick). See §6 for the data path and the events upgrade.

### 3.3 The two-column "active row" contract

For the **selected** session row:

```
[bullet] [short-id (8 hex)]   │   [last summarized message — single line, truncated to width]
```

- **Left column** is the session's short ID. (Full UUID available on `?`/detail.)
- **Right column** is the **last summarized message** for that session — a
  one-line, human-readable description of what the session is currently doing
  (e.g. "Running tests — 12 passed, fixing flaky timeout", or
  "Awaiting approval: write to .github/workflows/…"). Source + fallbacks: §6.
- The controller row's right column is a synthetic status
  (`idle · N delegations · ready` / `thinking…` while a coordinator turn is in
  flight).

### 3.4 Keybindings

| Key | Action |
|---|---|
| printable | edit input buffer |
| `Enter` | submit input (coordinator chat or routed command, §5) |
| `↑` / `↓` (empty input) | move session selection |
| `↑` / `↓` (non-empty input) | input history recall (reuse `CommandBar`) |
| `k` / `j` | move session selection (vim) |
| `PageUp` / `PageDn` | scroll session list |
| `Enter` on a selected session (input empty) | "focus": pre-fill `@<prefix>: ` into the input |
| `/` | begin a slash command (input gains leading `/`) |
| `Esc` | close any open modal; else clear input |
| `?` | help overlay (lists slash commands + keys) |
| `Tab` | (reserved) cycle focus input ⇄ list |
| `q` / `Ctrl-C` | quit (restore terminal) |

---

## 4. Architecture

### 4.1 Where it lives & how it launches

- **Module:** evolve `crates/trusty-mpm/src/tui/` (the coordinator dashboard) in
  place. Add a `coordinator` submodule tree under `tui/` for the new layout +
  slash-command modals; keep the health screen as-is.
- **Entry point:** add a `tm coordinator` subcommand (alias the existing `tm tui`
  to it, or make `coordinator` the new default surface and keep `tui` as a thin
  alias for backward compatibility). Binary name `tm` / `trusty-mpm`
  (`src/bin/tm/main.rs`). Reuse `resolve_daemon_url` for `--url`, and the
  `--interval-ms` flag for the refresh cadence.
- **Feature flag:** stays behind the existing `tui` feature
  (`ratatui` + `crossterm`, both optional).

### 4.2 Reuse from `open-mpm` (specific modules to port)

`open-mpm` (single crate at `/Users/masa/Projects/open-mpm`) already shipped a
mature ratatui REPL TUI with slash-command dispatch and its own tmux session
manager. The directly-portable pieces:

| `open-mpm` source | What it gives us | How it maps here |
|---|---|---|
| `src/repl/tui.rs` — `run_tui`, `setup_terminal`/`restore_terminal`, `event_loop`, the dedicated **crossterm key-reader thread** + `mpsc` channel | RAII terminal boundary (panic-safe restore), non-blocking key+resize+mouse intake, render-the-whole-frame loop | Port as the coordinator TUI's run/loop skeleton (trusty-mpm `tui/event_loop.rs` already has a leaner version — adopt the key-reader-thread + mpsc + resize/mouse handling). |
| `src/repl/tui.rs` — `ReplHandler` trait + `ReplEvent` enum | A clean "events mutate state; handler dispatches input" seam | Define a `CoordinatorEvent` enum (Key/Resize/Scroll/Submit/SessionsRefreshed/CoordinatorReply/OpenModal/…) and a handler trait so HTTP/poll work is testable off-terminal. |
| `src/repl/tui.rs` — `draw_picker`, `centered_rect`, `PickerState`, `PickerKind`, `SetChoices`/inline-choice picker | **The slash-command modal pattern** (centered overlay, `Clear`, bordered list, ↑↓/Enter/Esc, footer hint) | This is the backbone of every slash-command TUI in §5 (`/new` form, `/sessions` picker, confirm dialogs). |
| `src/repl/commands.rs` — `try_handle_slash` jump table | A proven slash-command dispatch shape (`Option<Result<(keep_looping, output)>>`) | Model the coordinator slash-command framework (§5.1) on this jump table. |
| `src/tm/` (`manager.rs`, `monitor.rs`, `project.rs`, `registry.rs`, `commands.rs`) — `TmManager`, `TmMonitor` (30 s reconcile), `capture_pane`, `SessionStatus`, `/tm` subcommands (`list/new/attach/kill/pause/resume/send/capture/status/reconcile`) | A reference session-model + **background monitor** + tmux capture + the exact slash subcommands we need | **Do not port the storage** (trusty-mpm has its own session manager + managed API). Port the **monitor/reconcile cadence idea** and the **subcommand surface** as the mapping target for trusty-mpm's managed endpoints. |
| `src/repl/dashboard`-style statusline (`statusline.rs`, `build_rich_statusline`) | The Claude-Code-like rich status strip | Optional polish for the top/bottom bar. |

> **Portability caveat.** `open-mpm` is `edition = "2021"`, a standalone crate
> with its own `crate::tm`, `crate::usage`, `crate::update` deps woven into
> `ReplApp`/`ReplStartup`. Port the **patterns and the widget code**, not the
> structs verbatim — strip the `open-mpm`-specific fields (usage accounting,
> ollama/provider switching, agent-scope) and retarget the handler at
> trusty-mpm's daemon client. The terminal-thread + mpsc + picker code is the
> high-value, low-coupling part.

### 4.3 State model

A single `CoordinatorApp` (owned by the render loop, snapshot-cloned per frame
like `open-mpm`'s `ReplApp`):

- `input: CommandBar` — edit buffer + history ring (reuse existing).
- `chat: Vec<ChatMessage>` — coordinator transcript (reuse existing
  `DashboardState` chat) — shown as a scrollback panel or folded into the list
  region (open question OQ3).
- `sessions: Vec<SessionRow>` — polled fleet, each carrying `id`, `prefix`,
  `status`, and **`last_summary: String`** (new — §6).
- `selected: usize` — active row (0 = controller).
- `modal: Option<Modal>` — open slash-command overlay (`NewSessionForm`,
  `SessionPicker`, `ConfirmKill { id }`, `Help`, …).
- `daemon_reachable: bool`, `coord_history` — reuse existing.

All HTTP/poll work happens in the handler/poller tasks and lands on the app via
`CoordinatorEvent`s through an `mpsc` channel; the render loop only reads state.

### 4.4 ratatui widget tree (per frame)

```
Frame
└─ Layout(Vertical)
   ├─ [0] input box      → Paragraph (prompt + buffer + cursor)
   ├─ [1] session list   → List<ListItem> with ListState (controller row + sessions;
   │                        selected row = two-column [id │ summary] Line)
   └─ [2] status bar     → Paragraph (slash hints + keys, reversed)
   └─ overlay (if modal) → centered_rect + Clear + bordered Block + body widget
```

---

## 5. Coordinator concept & slash commands

### 5.1 The coordinator (top input)

The input talks to a **coordinator / PM-like agent**. Two routing modes
(existing `POST /api/v1/sessions/chat` semantics, `coordinator.rs`):

- **Free text** → the coordinator LLM answers using a snapshot of all sessions
  (`build_coordinator_context`). Requires `OPENROUTER_API_KEY`; when absent the
  TUI shows the existing "coordinator chat is not configured" note and free-text
  turns are disabled (but slash commands and fleet visibility still work — G5).
- **`@<prefix>: <text>`** → routed straight at a named session (no LLM needed).

The coordinator is the layer that "can create + manage sessions." In the TUI,
**session creation/management is expressed as slash commands** (§5.2) that call
the session-manager managed API directly — i.e. the slash commands are the
deterministic control path; the LLM coordinator is the conversational/answer
path. (Future: let the LLM coordinator itself emit `/new`-style tool calls;
OQ4.)

Relationship to the **session-manager daemon** and the **driver**: the TUI is a
thin client of the daemon's managed API (§6); the daemon owns spawn/observe/
answer/stop/resume/decommission. The **driver** (`session-manager-driver` skill;
`crate::driver`) owns raw-pane inference for activity/state — the TUI consumes
its output via `/activity`, it does not re-implement inference.

### 5.2 Slash-command framework + commands

A slash command opens a **focused modal/panel** (the `open-mpm` picker pattern,
§4.2). Dispatch is a `try_handle_slash`-style jump table. Each command names the
managed endpoint it calls.

| Command | Modal / panel | Behavior | Session-manager API |
|---|---|---|---|
| `/help` | Help overlay | List all slash commands + keybindings. | none |
| `/new` | **New-session form** (small TUI): fields for **repo** (URL/path), **git ref/branch**, **task**, **runtime** (`claude-code` / `tcode`), optional **name hint**; Tab between fields, Enter submits, Esc cancels. | Spawn a managed session; on success select the new row and re-poll. Surface spawn errors inline (e.g. trust-dialog / headless-spawn blocker, #1269). | `POST /api/v1/sessions/managed` (`SpawnParams { repo_url, git_ref, task, name_hint, runtime }`). |
| `/sessions` | **Session picker** (list overlay) | List managed sessions; ↑↓ select, Enter = focus that row in the main list (and/or pre-fill `@prefix:`). | `GET /api/v1/sessions/managed` (or coordinator context). |
| `/attach` | (acts on selected/typed session) | Show/copy the tmux attach command, or suspend the TUI and `exec` the attach in the current terminal (OQ5). | `GET /api/v1/sessions/managed/{id}/attach-cmd`. |
| `/stop` | Confirm dialog | Stop the **runtime** only (keep workspace; session endures, resumable). | `POST /api/v1/sessions/managed/{id}/runtime-stop`. |
| `/resume` | (acts on selected/typed session) | Resume a stopped session (respawn runtime in its workspace). | `POST /api/v1/sessions/managed/{id}/resume`. |
| `/kill` | Confirm dialog (destructive, terminal) | Full teardown / decommission (kills runtime, removes workspace; no further resume). | `POST /api/v1/sessions/managed/{id}/decommission`. |

Supporting actions reachable from the picker/selected row (not necessarily
top-level slash commands): **send** text into a pane
(`POST …/{id}/send`) and **answer** a pending decision
(`POST …/{id}/answer`) — the latter is how the operator clears an
`AwaitingApproval` session from inside the TUI.

Each slash command, on completion, emits a "re-poll now" event so the session
list reflects the mutation immediately (§3.2).

---

## 6. Data sources & live updates

### 6.1 Session list + statuses

Two viable feeds (the TUI already uses the first):

1. **Coordinator context** — `GET /api/v1/sessions/context` returns
   `sessions: [CoordinatorSession { id, name, prefix, workdir, status,
   active_delegations, recent_output }]`. Lifecycle status words map to
   `core::session::SessionStatus` (`Starting`, `Active`, `AwaitingApproval`,
   `Detached`, `Paused`, `Stopped`) — see `tui/mod.rs::coordinator_session_to_row`.
2. **Managed list** — `GET /api/v1/sessions/managed` returns the
   `SessionRecord`s (`ManagedSessionState`, `pending_decision`,
   `proposed_default`, `last_activity_at`, …). Richer for management actions.

Recommendation: keep the coordinator-context feed for the list, and use the
managed endpoints for the slash-command actions.

### 6.2 "Last summarized message" — DATA-SOURCE FINDING

**A `summary` field exists, but not where the list needs it.**

- The **per-session** endpoint `GET /api/v1/sessions/managed/{id}/activity`
  (`ActivityResponse`) returns a `summary: String` —
  *"Human-readable summary of what the session is doing (from LLM or fallback)"* —
  plus `state`, `confidence`, `classification: Option<String>`, and the raw
  `raw_pane` (last 60 lines). The LLM fields are populated **only when
  `OPENROUTER_API_KEY` is configured**; otherwise `summary` is a non-LLM
  **fallback** and `classification` is `null`. The driver
  (`session-manager-driver`) does raw-pane inference **without** an LLM key.
- The **coordinator-context** list endpoint does **NOT** carry a summary — its
  `CoordinatorSession` only has `recent_output: Vec<String>` (a 20-line pane
  tail). The TUI's `SessionRow` likewise has no summary field today.

**Therefore the "ID | last summarized message" column needs work — it does not
exist on the list feed yet.** Options (pick in the design ticket, §11 child #3):

- **(A) Enrich the list feed (preferred):** add a `last_summary: String` to the
  coordinator-context `SessionSummary`/`CoordinatorSession`, populated daemon-side
  from the same classifier/fallback that `/activity` uses (cache it on the record
  so the list stays cheap). One round-trip per poll; LLM-optional.
- **(B) Per-session fan-out:** the TUI calls `/activity` for each session each
  refresh. Simple, but N+1 round-trips and re-runs classification — heavier.
- **(C) Client-side fallback only:** derive a one-liner from `recent_output`
  (last non-empty meaningful line) when no daemon summary exists — always works,
  no LLM. Good as the **fallback tier** under (A).

Recommended layering: **(A) with (C) as the no-key fallback.** The active-row
right column shows the daemon `summary` when present, else a client-derived last
meaningful line from `recent_output`, else the status word.

### 6.3 Live updates & the daemon URL convention

- **Polling now:** timer-based via `--interval-ms` (`poll_daemon`), plus an
  immediate re-poll after each mutating action.
- **Events later:** §11 child #6 proposes a push/event channel (the daemon
  already ingests hook events — `recent_events` in `CoordinatorContext`; a future
  SSE/websocket would let the list update on activity instead of on a timer).
- **Daemon URL convention (bug #1268).** Resolve via `resolve_daemon_url`:
  explicit `--url`/`TRUSTY_MPM_URL` → `~/.trusty-mpm/daemon.lock` → default. Note
  the **port mismatch hazard #1268**: the framework instruction template
  references `${TRUSTY_MPM_URL:-http://localhost:7799}`
  (`core/instruction_pipeline.rs`) while `DEFAULT_DAEMON_URL` is
  `http://127.0.0.1:7880` (`core/discovery.rs`). The TUI must **always** go
  through `resolve_daemon_url` (never hard-code a port) and **self-heal** by
  re-resolving from the lock file on a failed poll (port the existing
  `rediscover_daemon`). The spec treats #1268 as an external dependency to land,
  not something the TUI papers over.

---

## 7. Dependencies & risks

| # | Item | Impact | Mitigation |
|---|---|---|---|
| R1 | **"Last summarized message" not on the list feed** (§6.2). | The headline two-column feature has no direct data source. | Child ticket #3: enrich coordinator-context with `last_summary` (option A) + client fallback (option C). Blocks the active-row column. |
| R2 | **Trust-dialog / headless-spawn blocker (#1269).** | `/new` may spawn a session that stalls on Claude Code's trust prompt; the TUI would show a session stuck in `Starting`/`AwaitingApproval`. | Depends on #1269; until then, `/new` must surface the spawn/attach state and let the operator `/attach` to clear the dialog manually. |
| R3 | **Daemon URL / port mismatch (#1268).** | TUI connects to the wrong port → "daemon unreachable". | Always use `resolve_daemon_url`; self-heal via lock file (§6.3); track #1268. |
| R4 | **LLM-key-gated features (G5).** | No `OPENROUTER_API_KEY` → no free-text coordinator answers and weaker summaries. | Degrade gracefully: slash commands + fleet list work without a key; summaries fall back to `recent_output`-derived lines. |
| R5 | **Session-manager API gaps.** | No bulk endpoints; per-session summary requires fan-out or enrichment; no push/events. | Enumerate in child #2/#3/#6; prefer enriching existing endpoints over N+1 calls. |
| R6 | **SLOC 500-cap** on new production files. | Big TUI files can't merge. | Split per §4.4 (input / list / modals / poll / events as separate modules under `tui/coordinator/`). |
| R7 | **`open-mpm` port drift.** | `open-mpm` structs carry unrelated fields (usage, ollama, agent-scope). | Port patterns/widgets, not structs verbatim (§4.2 caveat). |

---

## 8. Behavior contract (summary)

- **Inputs:** keystrokes; daemon poll responses (`sessions/context`,
  `sessions/managed*`, `…/activity`); the daemon URL (resolved).
- **Outputs:** a rendered full-screen TUI; `POST`s to coordinator-chat and
  managed-lifecycle endpoints; a restored terminal on exit (always, even on
  panic/error).
- **Preconditions:** the `tm` daemon is reachable (or becomes reachable;
  unreachable state is rendered, not fatal).
- **Postconditions:** fleet mutations issued by the TUI are reflected in the list
  within one refresh; the terminal is restored on quit.
- **Error conditions:** unreachable daemon → "daemon unreachable" + URL
  re-resolution; spawn/stop/kill errors → inline modal error, no crash; missing
  LLM key → free-text disabled with a note, management still works.

---

## 9. Test strategy

- **Pure/unit (no terminal, no network):** line-builders for the controller
  bullet, per-session rows, and the active two-column `id │ summary` row;
  selection clamp on refresh; slash-command parse/dispatch jump table; the
  client-side `recent_output → last_summary` fallback; URL resolution/self-heal.
  (Mirror the existing `tui/dashboard/tests.rs`, `tui/tests.rs`,
  `tui/client.rs` tests.)
- **Modal logic:** `NewSessionForm` field navigation/validation; confirm-dialog
  gating for `/kill`.
- **Integration (`#[ignore]` / behind a daemon):** spawn → appears in list →
  `/stop` → `Stopped` → `/resume` → `Active` → `/kill` → gone, against a live
  managed API.
- **Terminal glue** (`run`/`event_loop`) stays thin and is exercised manually /
  via a tmux harness (cf. `open-mpm`'s `scripts/tmux-repl-test.sh`).

---

## 10. Out of scope

- Editing code or files inside a session (that is the session's own Claude Code).
- A web/graphical UI — owned by `trusty-console` / `trusty-mpm-gui`.
- Replacing the `tm session …` / `tm` CLI (the TUI complements it).
- Implementing the daemon-side `last_summary` enrichment (that is its own child
  ticket; this spec only specifies the contract the TUI needs).
- Fixing #1268 / #1269 (external dependencies the TUI consumes).
- Multi-coordinator / multi-user concurrency.

---

## 11. Open questions

- **OQ1.** Input **on top of** the list (this mockup) vs. Claude-Code-exact
  (composer at the **bottom**, list above)? Brief says "input box; BELOW the
  input, a list" → input-on-top. Confirm.
- **OQ2.** Is the coordinator **chat transcript** a separate scrollback panel, or
  do coordinator replies fold into the list region / a transient area? (Today
  `tui/` has a dedicated chat pane.)
- **OQ3.** `/attach` behavior: copy the attach command vs. suspend-and-`exec`
  tmux attach in the current terminal (and restore the TUI on detach)?
- **OQ4.** Should the LLM coordinator be able to **emit `/new`-style actions**
  (tool-calling) so "spin up a session for #412" works as free text, not just as
  an explicit `/new`?
- **OQ5.** Refresh cadence default and whether to ship events (#6) in v1 or keep
  polling-only for the MVP.
- **OQ6.** Naming: `tm coordinator` as the new primary, with `tm tui` aliased —
  or keep `tm tui` and add the layout under it?

---

## 12. References

- Existing TUI: `crates/trusty-mpm/src/tui/` (`mod.rs`, `dashboard/mod.rs`,
  `client.rs`, `event_loop.rs`, `health/`).
- Coordinator: `crates/trusty-mpm/src/daemon/coordinator.rs`,
  `.../daemon/api/coordinator_routes.rs`,
  `crates/trusty-mpm/src/client/http_client/types.rs` (`CoordinatorSession`,
  `CoordinatorContext`, `CoordinatorChatOutcome`).
- Managed session API: `crates/trusty-mpm/src/daemon/managed_routes/`
  (`mod.rs`, `lifecycle.rs`), `crates/trusty-mpm/src/session_manager/`
  (`record.rs`, `manager.rs`).
- URL resolution: `crates/trusty-mpm/src/core/discovery.rs`.
- `open-mpm` prior art: `/Users/masa/Projects/open-mpm/src/repl/tui.rs`,
  `.../repl/commands.rs`, `.../repl/dispatch.rs`, `.../tm/` (manager, monitor,
  project, registry, commands).
- Skill: `session-manager-driver` (raw-pane inference, spawn→observe→answer→stop
  loop).
- Issues: **#1268** (daemon URL vs bind port), **#1269** (trust-dialog /
  headless-spawn blocker).

---

## 13. Ticket Breakdown

**EPIC — Coordinator TUI (`tm coordinator`): Claude-Code-like input + live
session list (controller + per-session, ID | summary) with in-TUI slash-command
session management.**
Evolve the existing `trusty-mpm/src/tui/` dashboard into a coordinator TUI:
input box over a live session list (controller bullet + one bullet per managed
session; active row in two columns `[id] | [last summarized message]`), driven
by a coordinator agent, with slash-command modals for full session lifecycle.
Reuse `open-mpm` ratatui prior art. Child tickets are small and independently
shippable.

---

### Child #1 — Coordinator TUI skeleton (input + session-list layout + controller bullet)
- **Scope:** New `tui/coordinator/` module + `tm coordinator` subcommand
  (alias `tm tui`). Vertical layout: input box (top, reuse `CommandBar`),
  session-list region (middle), status bar (bottom). Render the **controller
  bullet** as row 0 always; render placeholder/managed session rows beneath it.
  Selection/highlight, ↑↓/`k`/`j` movement, scrolling, `q`/`Ctrl-C` quit with
  panic-safe terminal restore. No live data yet (static/empty list ok).
- **Dependencies:** none (uses existing `tui` feature + deps).
- **Acceptance:** `tm coordinator` launches; input editable; controller bullet
  shown as row 0; selection moves and highlights; quit restores the terminal;
  unit tests cover row-building + selection clamp; `cargo clippy` clean; new
  files ≤500 SLOC.

### Child #2 — Session-list data wiring from the session-manager API
- **Scope:** Poll `GET /api/v1/sessions/context` (reuse `poll_daemon` /
  `coordinator_session_to_row`) to fill the list under the controller bullet;
  map status words to `SessionStatus`; bullet glyph/color by status
  (incl. `AwaitingApproval`); selection-by-ID survives refresh; render the
  active row's **left column** (short id) and a dimmed status right column
  (placeholder until #3). Self-heal daemon URL via `resolve_daemon_url` +
  `rediscover_daemon`; show "daemon unreachable" without crashing.
- **Dependencies:** #1; aware of #1268.
- **Acceptance:** with the daemon running, live sessions appear under the
  controller; statuses + bullets correct; killing/adding a session updates the
  list within one interval; unreachable daemon renders gracefully; unit tests for
  the projection + clamp; clippy clean.

### Child #3 — "Last summarized message" source (the right column)
- **Scope:** Implement the active-row right column. **Daemon side:** add a
  `last_summary` to the coordinator-context session summary
  (`coordinator.rs` `SessionSummary` + `CoordinatorSession`), populated from the
  classifier/fallback (cache on the record; LLM-optional). **Client side:**
  render `last_summary`; when absent, derive a one-line fallback from
  `recent_output` (last meaningful line), else the status word.
- **Dependencies:** #2. (Touches the daemon — coordinate with session-manager
  owners; run dependent tests per CLAUDE.md cross-crate rules.)
- **Acceptance:** selected session shows `[id] │ [summary]`; works **with and
  without** `OPENROUTER_API_KEY` (LLM summary vs derived fallback); daemon-side
  field covered by a deserialize/build test; client fallback unit-tested; clippy
  clean.

### Child #4 — Slash-command framework + `/new` + `/sessions` (+ `/help`)
- **Scope:** Port the `open-mpm` picker/modal pattern (`draw_picker`,
  `centered_rect`, picker state) and a `try_handle_slash`-style jump table.
  Implement `/help` (overlay), `/sessions` (picker → focus a row), and `/new`
  (multi-field form: repo, git ref, task, runtime, name hint → `POST
  /api/v1/sessions/managed`). Re-poll immediately after `/new`; surface spawn
  errors inline.
- **Dependencies:** #2 (and #3 for the new row's summary, soft).
- **Acceptance:** `/help` lists commands; `/sessions` focuses a session; `/new`
  spawns a session that then appears in the list; form navigation (Tab/Enter/Esc)
  + validation unit-tested; dispatch jump table unit-tested; clippy clean; files
  ≤500 SLOC.

### Child #5 — Lifecycle slash commands: `/attach`, `/stop`, `/resume`, `/kill`
- **Scope:** `/attach` → `GET …/{id}/attach-cmd` (show/copy or suspend-exec, per
  OQ3); `/stop` → `POST …/{id}/runtime-stop` (confirm); `/resume` →
  `POST …/{id}/resume`; `/kill` → `POST …/{id}/decommission` (destructive
  confirm). Plus pane **send** and decision **answer** from the selected row
  (`…/{id}/send`, `…/{id}/answer`) to clear `AwaitingApproval`. Re-poll after
  each.
- **Dependencies:** #4 (modal framework).
- **Acceptance:** integration loop spawn → `/stop` (Stopped) → `/resume`
  (Active) → `/kill` (gone) against a live daemon; destructive `/kill` requires
  confirm; errors render inline; unit tests for command→endpoint mapping +
  confirm gating; clippy clean.

### Child #6 — Live refresh / events
- **Scope:** Finalize refresh cadence (`--interval-ms`) and the immediate
  re-poll-after-mutation seam; spike/implement a push channel (SSE/websocket over
  the daemon's hook-event stream) so the list updates on activity instead of only
  on a timer. Mouse-wheel + `PageUp/Dn` scroll (port from `open-mpm`).
- **Dependencies:** #2 (#5 nice-to-have for mutation re-poll points).
- **Acceptance:** list reflects external session activity without a manual key;
  mutation actions update within one tick or immediately; if events ship, a
  reconnect/backoff path exists; cadence + scroll behaviors unit/perception
  tested; clippy clean.

### Child #7 — Port `open-mpm` TUI widgets / scaffolding (hardening)
- **Scope:** Consolidate the run/loop on the `open-mpm` skeleton: dedicated
  crossterm **key-reader thread** + `mpsc` `CoordinatorEvent` channel, resize
  handling, panic-safe `setup_terminal`/`restore_terminal`, and the shared
  picker/centered-rect widgets factored into one module. Remove any duplicated
  ad-hoc event handling left from #1.
- **Dependencies:** #1, #4 (modal widgets), ideally after #5.
- **Acceptance:** single event-loop path; resize/mouse/key all flow through the
  channel; terminal always restored on panic; no duplicated widget code; clippy
  clean; files ≤500 SLOC (split if needed).
