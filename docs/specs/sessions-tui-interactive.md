# DOC-16 — Interactive Sessions TUI (`tm sessions tui`)

**Status:** Draft
**Subsystem:** trusty-mpm — TUI / sessions
**Owner:** Engineering (trusty-mpm)
**Last-updated:** 2026-06-17
**Spec ID:** `SPEC-SM-TUI-01~draft` (DOC-16)
**Builds on:** DOC-13 — Coordinator TUI (`docs/specs/tui-coordinator.md`, PR #1271),
DOC-14 — Session Manager (SM) Agent (`docs/specs/session-manager-agent.md`)
**Cross-ref:** existing coordinator TUI modules (`crates/trusty-mpm/src/tui/coordinator/`),
session-manager managed API (`crates/trusty-mpm/src/daemon/managed_routes/`),
coordinator endpoints (`crates/trusty-mpm/src/daemon/coordinator.rs`,
`.../daemon/api/coordinator_routes.rs`), the `session-manager-driver` skill, the
trusty-memory palace engine (`crates/trusty-common/src/memory_core/`), and issues
**#1272** (SM TUI epic), **#1275** (daemon `last_summary`), **#1276** (slash
framework), **#1277** (lifecycle cmds), **#1278** (live refresh/scroll), **#1279**
(hardening), **#1268** (daemon URL vs bind port), **#1269** (trust-dialog /
headless-spawn blocker).

> **Scope note.** This is a **behavior + UX requirements** spec for the
> *interactive* sessions TUI — the Claude-Code-style operator surface launched by
> `tm sessions tui`. It is the **authoritative spec the #1272 children implement
> against**. It specifies what the TUI must do, how it looks, which daemon APIs it
> consumes, and the daemon-side prerequisites it depends on. It does **not**
> implement the TUI. The PR that carries this doc opens **no** Rust changes.

---

## 1. Purpose & Scope

### 1.1 What we are building

An interactive, Claude-Code-style **sessions TUI** for `trusty-mpm` (`tm sessions
tui`). It evolves the read-only coordinator dashboard (DOC-13, `tui/coordinator/`)
into a fully interactive surface: a startup banner that confirms the trusty
backplane is live, a scrollable **numbered** session list, a rich **statusline**
(model, tokens, session cost, daily cost), an **expandable multi-line input box**,
and per-session **summary transcripts** that grow as sessions report progress —
including a **blinking** bullet for any session that is actively summarizing.

The operator talks to the fleet two ways: by **filtering to a session** (arrow +
Enter) or by **addressing a session inline** (`/<session#> <message>`). User
messages render in a distinctive style directly below the relevant session summary
bullet. The TUI can **create** and **terminate** sessions (with a confirmation
warning when the target is active), and forwards a raw **Esc** to a focused
session for stop/interrupt.

### 1.2 Goals

- **G1 — Backplane-confirmed startup.** A banner names the tool + version and
  confirms trusty-memory and trusty-search are active, using a **fixed user-level
  palace named `user`** (D4).
- **G2 — Numbered, scrollable fleet view.** A numbered session list (≥5 visible
  lines) with project ID/name, per-session status glyphs, and a blinking bullet
  while a session is actively summarizing (D1).
- **G3 — Rich statusline.** Model, tokens up/down, session cost, and **daily
  cost** (daemon-persisted, D2).
- **G4 — Two interaction modes.** Filter-to-session (arrow+Enter) and inline
  addressing (`/<session#> <message>`), both delivered via the managed send API
  (D3).
- **G5 — Growing summary transcript.** Per session, append a new summary bullet
  when new daemon-produced summary content arrives; render user input below the
  relevant bullet; the exact summary line format is fixed (§4.3).
- **G6 — Precise Esc semantics.** Esc forwards the raw escape sequence to the
  focused session; **exiting** the filter is a separate, documented key (§5.5).
- **G7 — Lifecycle from the TUI.** Create new / terminate sessions, with an
  active-session confirmation warning; termination uses a **per-adapter
  termination sequence** (§5.6, §6.3).

### 1.3 Non-goals (see §9)

Editing code inside a session; a web/graphical UI (that is `trusty-console` /
`trusty-mpm-gui`); replacing the `tm` CLI; implementing the daemon-side inference
(DOC-14 owns the SM brain); building the EventStream push channel (the live-refresh
follow-up, §9).

---

## 2. Background: current state

A read-only coordinator dashboard already ships under
`crates/trusty-mpm/src/tui/coordinator/` (DOC-13, PR #1271), behind the `tui`
feature (`ratatui` 0.29 + `crossterm` 0.28, shared via `[workspace.dependencies]`).
Today it provides:

| Capability | Where | Notes |
|---|---|---|
| Input box + session-list layout + controller bullet | `tui/coordinator/` | Renders a **read-only** list today. |
| Poll `GET /api/v1/coordinator/context` → session rows | `tui/mod.rs::poll_daemon`, `tui/client.rs` | Timer-based (`--interval-ms`). |
| Send free text / `@prefix:` to `POST /api/v1/coordinator/chat` | `tui/mod.rs::coordinator_send` | LLM or routed. |
| Managed session API (spawn/observe/stop/resume/decommission/send) | `crates/trusty-mpm/src/daemon/managed_routes/` | The lifecycle + injection surface this spec consumes. |
| Self-healing daemon URL re-resolution | `tui/mod.rs::rediscover_daemon`, `core/discovery.rs` | Re-reads `~/.trusty-mpm/daemon.lock`. |

**Gaps versus this spec.** The current surface is read-only and has no: (a)
backplane startup banner + memory/search probes; (b) numbered list with a blinking
"summarizing" bullet; (c) statusline cost / daily-cost; (d) per-session **summary
transcript** that grows over time with user-input rendering interleaved; (e)
session-filter view with raw-Esc forwarding; (f) per-adapter termination sequence;
(g) expandable multi-line input box. These are the net-new scope this spec governs.

---

## 3. UX & Layout

### 3.1 Startup banner & line {#SPEC-SM-TUI-01~draft}

**STUI-0 — Startup banner + active-session line.**

On launch, before the interactive view, the TUI prints a banner:

```
trusty-mpm sessions  v<CARGO_PKG_VERSION>
  memory  ● active   (palace: user)
  search  ● active
3 active sessions  ·  type /help for commands
```

- **Behavior contract.**
  - **Inputs:** the resolved daemon URL; trusty-memory health + `user` palace
    presence (D4); trusty-search health; the managed-session list count.
  - **Outputs:** a banner line `trusty-mpm sessions v<version>` where `<version>`
    is the crate `CARGO_PKG_VERSION` (compile-time `env!("CARGO_PKG_VERSION")`); two
    backplane status lines (memory, search) each with a glyph (`●` active / `○`
    unreachable); a startup line showing the **count of active sessions** plus a
    `/help` hint.
  - **Preconditions:** none — an unreachable backplane is *rendered* (`○ unreachable`),
    not fatal.
  - **Postconditions:** the interactive view opens with the input box focused.
  - **Error conditions:** memory/search unreachable → `○ unreachable` glyph + a
    one-line hint; the TUI still opens (degrades, never aborts).

### 3.2 Screen layout (ASCII mockup)

```
┌─ trusty-mpm sessions ─────────────────── v0.x.y · daemon ● http://127.0.0.1:7880 ┐
│ SESSIONS (3)                                            [↑↓] select  [↵] filter   │
│                                                                                   │
│  1. ● 4f9c…a1  aipowerranking   Running tests — 12 passed, fixing flaky timeout   │
│  2. ◌ 7b2e…c0  genealogy        Awaiting approval: write to .github/workflows/…   │  ← ◌ blinks while summarizing
│  3. ○ d1a8…ff  smarterthings    Idle — last activity 6m ago                       │
│                                                                                   │
│                                                                                   │  ← ≥5 visible list lines reserved
├───────────────────────────────────────────────────────────────────────────────────┤
│ model claude-sonnet-4-6 · ↑12.4k ↓3.1k tok · session $0.21 · today $4.87          │  ← statusline
├───────────────────────────────────────────────────────────────────────────────────┤
│ › spin up a session on aipowerranking for ticket #412 and watch it_               │  ← multi-line input (expands)
│   /help · /<n> <msg> address session n · ↵ send · q quit                          │  ← key hints
└───────────────────────────────────────────────────────────────────────────────────┘
```

Layout regions (ratatui vertical `Layout`):

1. **Session list** (`Constraint::Min(5)`, **≥5 visible lines reserved**): the
   scrollable, **numbered** fleet. Each row: `<n>. <glyph> <short-id> <project>
   <one-line status>` (§3.3). Numbers are stable per-poll ordinals (1-based) used
   by inline addressing (`/<n> <msg>`, §5.2).
2. **Statusline** (`Constraint::Length(1)`): model, tokens up/down, session cost,
   daily cost (§3.4).
3. **Input box** (`Constraint::Min(3)`, **expandable**): a multi-line composer
   that grows with content up to a cap, then scrolls internally (§3.5).
4. **Key-hint line** (`Constraint::Length(1)`): contextual hints.

When a session **filter** is active (§5.4) the list region is replaced by that
session's **summary transcript** (summary bullets + interleaved user input).

### 3.3 Per-session row & status bullets

**STUI-3 — Status bullets + blinking summarizing glyph.**

| Glyph | Meaning |
|---|---|
| `●` | Active / running |
| `◌` (**blinking**) | Actively **summarizing** (the daemon's `summarizing` signal is true, D1) |
| `◍` | Awaiting approval / pending decision |
| `○` | Idle / paused / stopped |

- **Behavior contract.**
  - **Inputs:** each session's `SessionStatus`, plus the daemon-exposed
    `summarizing: bool` in-progress signal (§6.2).
  - **Outputs:** the row glyph; when `summarizing` is true the bullet **blinks**
    (ratatui `Modifier::SLOW_BLINK`, or a TUI-side toggle on the refresh tick), and
    the row shows the **number + id** so the operator can address it mid-summary.
  - **Preconditions:** none.
  - **Postconditions:** the blink clears on the first poll where `summarizing` is
    false; the new summary bullet (§4.3) is appended in the same refresh.
  - **Error conditions:** missing `summarizing` field (older daemon) → never blink
    (treat as false).

Status words map to `core::session::SessionStatus`
(`Starting`/`Active`/`AwaitingApproval`/`Detached`/`Paused`/`Stopped`) — see
`tui/mod.rs::coordinator_session_to_row` (DOC-13 §6.1).

### 3.4 Statusline (model, tokens, costs)

**STUI-2 — Statusline with costs + daily cost.**

```
model <id> · ↑<up> ↓<down> tok · session $<session_cost> · today $<daily_cost>
```

- **Behavior contract.**
  - **Inputs:** the SM/active model id; cumulative tokens up/down for the session;
    the running session cost; the daemon-persisted **daily cost** (D2, §6.2).
  - **Outputs:** a single statusline. Token counts are human-formatted (`12.4k`);
    costs are USD to cents.
  - **Preconditions:** none — when a field is unknown (e.g. no inference
    configured) it renders `—`.
  - **Postconditions:** values refresh on each poll tick and immediately after a
    cost-bearing turn.
  - **Error conditions:** daily-cost endpoint unavailable → render `today —` (never
    block the frame).

### 3.5 Expandable multi-line input box

**STUI-10 — Expandable multi-line input.**

- **Behavior contract.**
  - **Inputs:** keystrokes; `Enter` (send), `Shift+Enter`/`Alt+Enter` (newline,
    grow the box).
  - **Outputs:** a composer that starts at one visible line and **expands**
    vertically as the operator adds lines, up to a cap (e.g. 8 lines) after which it
    scrolls internally; the cursor and a `›` prompt are shown; history recall on
    `↑`/`↓` when the buffer is empty (reuse `CommandBar`).
  - **Preconditions:** input focus.
  - **Postconditions:** on `Enter` the buffer is routed per §5 and cleared; the box
    collapses back to one line.
  - **Error conditions:** an empty submit is a no-op.

### 3.6 Selection, scrolling, refresh

- **Selection.** Exactly one numbered row is selected; `↑`/`↓` (and `k`/`j`) move
  it; selection survives a refresh by session id (clamp if it disappears — mirror
  `DashboardState::clamp_selection`).
- **Scrolling.** When sessions exceed the visible region the list scrolls with the
  selection (`ListState` offset); `PageUp`/`PageDn` jump a page; mouse-wheel
  scrolls. (#1278.)
- **Live refresh.** Timer-based (`--interval-ms`, default ~1500 ms) **and** an
  immediate re-poll after any mutating action (create/terminate/send) so the view
  reflects the change without waiting for the next tick (#1278).

### 3.7 Keybindings

| Key | Context | Action |
|---|---|---|
| printable | input focus | edit input buffer |
| `Shift+Enter` / `Alt+Enter` | input focus | insert newline (grow box, §3.5) |
| `Enter` | input non-empty | route + send (§5) |
| `↑` / `↓`, `k` / `j` | input empty | move session selection |
| `↑` / `↓` | input non-empty | input history recall |
| `Enter` | input empty, row selected | **filter** to that session (§5.4) |
| `/<n> <msg>` | input | inline-address session `<n>` (§5.2) |
| `PageUp` / `PageDn` | — | scroll list / transcript |
| `Esc` | **in a session filter** | forward raw `\x1b` to the focused session (§5.5) |
| `q` (or second `Esc`) | **in a session filter** | exit the filter back to the list (§5.5) |
| `Esc` | not filtered | clear the input buffer |
| `/help` | input | help overlay |
| `q` / `Ctrl-C` | not filtered | quit (restore terminal) |

---

## 4. Summary transcript model

### 4.1 Per-session summary tracking

**STUI-5 — Growing summary transcript + user-input rendering.**

Each session carries a transcript the TUI maintains client-side from the daemon's
cached **`last_summary`** (D1, §6.2):

- The TUI tracks, per session, the **last-summarized line** it has already
  rendered.
- On each poll, when the daemon's `last_summary` for a session **differs** from the
  last line the TUI rendered for that session (and the session is the source of new
  content), the TUI **appends a NEW summary bullet** to that session's transcript.
  Identical `last_summary` values do **not** append (no duplicate bullets).
- User-entered messages addressed to a session render **below** the relevant
  summary bullet in a distinctive **"user input" style** (e.g. right-marked, dim
  cyan, prefixed `you ›`).

### 4.2 Behavior contract

- **Inputs:** per-poll `last_summary: String` per session (§6.2); operator messages
  sent to a session (§5).
- **Outputs:** an append-only transcript per session: alternating summary bullets
  and user-input lines, in arrival order.
- **Preconditions:** the session exists in the current poll.
- **Postconditions:** a new bullet is appended **iff** `last_summary` changed since
  the last rendered line for that session; a sent user message is appended below
  the most recent summary bullet for the addressed session.
- **Error conditions:** `last_summary` empty/unset → no bullet appended; a fallback
  last-line may be shown **only** when inference is unconfigured, clearly marked
  (D1).

### 4.3 Summary line format (EXACT — NORMATIVE)

A summary bullet renders **exactly**:

```
<session#>.<session-id>: <summary>
```

- `<session#>` — the 1-based ordinal from the numbered list (§3.2).
- `<session-id>` — the session's short id (first 8 hex of the UUID).
- `<summary>` — the daemon `last_summary` text, single line, truncated to width
  with an ellipsis.

Example: `1.4f9ca1: Running tests — 12 passed, fixing flaky timeout`.

---

## 5. Interaction model

### 5.1 Routing overview

Two ways to address a session, plus list-level navigation:

1. **Filter to a session** — arrow-select a numbered row + `Enter` (§5.4).
2. **Inline address** — type `/<session#> <message>` into the input (§5.2).
3. **Free text / slash commands** — non-addressed input routes to the SM/coordinator
   chat (DOC-14) or a lifecycle slash command (§5.6).

### 5.2 Inline addressing — `/<session#> <message>`

**STUI-6 — Inline session addressing.**

- **Behavior contract.**
  - **Inputs:** an input buffer matching `^/(\d+)\s+(.+)$`.
  - **Outputs:** `<message>` is delivered to session `<session#>` via the managed
    send API (D3); a user-input line is appended below that session's latest summary
    bullet (§4.1).
  - **Preconditions:** `<session#>` is a valid current ordinal and the session is a
    **managed** session (D3 prerequisite).
  - **Postconditions:** an immediate re-poll; the transcript shows the user line.
  - **Error conditions:** out-of-range `<session#>` → inline error, input retained;
    non-managed session → "session is not managed; cannot inject" notice.

> **Disambiguation.** `/<digits> <text>` is inline addressing; `/<word> …`
> (e.g. `/help`, `/new`) is a slash command (§5.6). A leading `/` followed by a
> non-digit is never treated as addressing.

### 5.3 User-input rendering style

User messages (from §5.2 inline addressing or §5.4 filter input) render in the
distinctive "user input" style below the relevant summary bullet (§4.1) — visually
distinct from daemon summary bullets so the transcript reads as a dialogue.

### 5.4 Filter-to-session view

**STUI-7 (part) — Filter view.**

- **Behavior contract.**
  - **Inputs:** `Enter` on a selected row with an empty input.
  - **Outputs:** the list region is replaced by the **filtered** transcript showing
    **ONLY** that session's summary bullets + interleaved user input (§4); the input
    box now sends to the focused session by default (no `/<n>` prefix needed).
  - **Preconditions:** a row is selected.
  - **Postconditions:** subsequent plain-text submits go to the focused session via
    the managed send API (D3); the header indicates the focused session.
  - **Error conditions:** focused session disappears from the fleet → the filter
    exits with a notice.

### 5.5 Esc semantics (PRECISE — NORMATIVE)

**STUI-7 (part) — Esc forwarding + filter exit.**

Esc has meaning **only within a session filter**:

- **In a filter:** a single `Esc` **forwards the raw escape sequence** (`\x1b`,
  one byte) to the focused session via the managed send API (D3) — i.e. it sends a
  stop/interrupt to the running program in the session's pane (e.g. cancels a Claude
  Code turn). It does **not** exit the filter.
- **Exiting the filter** is a **separate key**: **`q`** (or a **second `Esc`**
  pressed within a short coalescing window, e.g. 400 ms, when the first Esc was
  already forwarded). The documented primary exit key is **`q`**; the double-Esc is
  a convenience alias.
- **Outside a filter:** `Esc` clears the input buffer (it does **not** forward to
  any session and does **not** quit).

- **Behavior contract.**
  - **Inputs:** `Esc` / `q` keystrokes; current filter state.
  - **Outputs:** in-filter `Esc` → `POST …/{id}/send` with body `"\x1b"` (D3);
    `q`/second-Esc → leave the filter, restore the list.
  - **Error conditions:** send failure → inline notice; filter state is preserved.

### 5.6 Lifecycle: create / terminate

**STUI-7 (part) — Create new / terminate with active-session confirmation.**

- **Create.** `/new` opens a small form (repo/path, git ref, task, runtime, name
  hint) → `POST /api/v1/sessions/managed`; on success select + re-poll. (Spawn
  errors, incl. #1269 trust-dialog, surface inline.)
- **Terminate.** `/stop` (runtime only) and `/kill` (full decommission) act on the
  selected/focused session. **If the target session is active, the TUI shows a
  confirmation warning** before sending the termination sequence.
- **Termination sequence (per-adapter, NORMATIVE concept).** Terminating a session
  sends that session **adapter's termination sequence** through the managed send API
  (D3) before/with the lifecycle call. Each adapter declares its own sequence via an
  adapter-trait method (§6.3). **For the mpm adapter the termination sequence is the
  literal `/mpm-session-pause`.**

- **Behavior contract.**
  - **Inputs:** the create form, or a terminate action on a target session; the
    target's active state; the adapter's termination sequence.
  - **Outputs:** create → `POST /api/v1/sessions/managed`; terminate → send the
    adapter termination sequence via `POST …/{id}/send` then the lifecycle endpoint
    (`runtime-stop` / `decommission`).
  - **Preconditions:** terminate of an **active** session requires explicit
    confirmation.
  - **Postconditions:** an immediate re-poll reflects the new/removed session.
  - **Error conditions:** spawn/terminate failure → inline modal error, no crash.

---

## 6. Daemon-side prerequisites

> These are the **daemon contracts the TUI consumes.** They are net-new daemon
> scope (their own child tickets, §10) — this spec only states the contract.

### 6.1 Summary inference (ties to #1275)

The per-session summary is produced by the **session-manager inference layer in
the daemon** (DOC-14 §3.6/§5.4), not by the TUI. The daemon **caches the latest
summary** on the coordinator-context session object and exposes it as
`last_summary` (#1275).

### 6.2 New fields on the coordinator-context session object

| Field | Type | Purpose | TUI use |
|---|---|---|---|
| `last_summary` | `String` | cached latest daemon-produced summary | summary bullet (§4.3) — #1275 |
| `summarizing` | `bool` | in-progress signal (a summary call is running) | blinking bullet (§3.3) — D1/#1275 |
| `tokens_up` / `tokens_down` | `u64` | cumulative session tokens | statusline (§3.4) |
| `model` | `String` | active model id | statusline (§3.4) |

Plus a **daily-cost endpoint** (D2): `GET /api/v1/cost/daily` →
`{ date: "YYYY-MM-DD", cost_usd: f64 }`, daemon-persisted to disk (§D2).

### 6.3 Per-adapter termination-sequence trait method (NORMATIVE)

Each session **adapter** declares its termination sequence via a trait method:

```rust
/// Why: terminating a session cleanly means telling the *runtime* to wind
/// down in its own idiom before the daemon tears down the pane/workspace.
/// What: returns the literal keystroke sequence the daemon injects (via the
/// managed send API) to ask this adapter's runtime to pause/exit.
/// Test: per-adapter unit test asserts the exact sequence.
fn termination_sequence(&self) -> &str;
```

- **mpm adapter:** returns the literal `"/mpm-session-pause"`.
- Other adapters (e.g. a raw `claude-code` adapter) return their own idiom (a
  spec'd default may be the raw `\x1b` interrupt, decided in the adapter ticket).

The daemon injects the returned sequence via `POST …/{id}/send` as part of the
terminate path; the TUI does **not** hard-code any adapter's sequence — it relies
on the adapter trait (the TUI's `/stop`/`/kill` call the lifecycle endpoints, and
the daemon applies the adapter sequence).

### 6.4 Input + termination delivery (D3)

All operator-to-session delivery (inline addressing §5.2, filter input §5.4, raw
Esc §5.5, and adapter termination sequences §6.3) goes through
`POST /api/v1/sessions/managed/{id}/send` (direct tmux injection). **Prerequisite:**
the target must be a **managed** session (the daemon owns its tmux pane). Sessions
that are merely discovered/adopted but not managed cannot receive injection — the
TUI surfaces this as a "not managed" notice (§5.2 error condition).

---

## 7. Design decisions (NORMATIVE)

### D1 — Summaries require inference (daemon-side)

Summaries are produced by the **session-manager's inference layer running in the
daemon** (DOC-14). The provider is configurable in the console dashboard;
supported providers: **OpenRouter, AWS Bedrock, OpenAI key, Anthropic key,
Ollama**. The daemon **caches the latest summary** (`last_summary`) and exposes a
**`summarizing`** in-progress signal so the TUI can blink (§3.3). A silent
client-only heuristic is **not** the product path: a last-line fallback **MAY** be
shown **only when inference is unconfigured**, and it must be **clearly marked**
(e.g. a `~` prefix or dim "(no inference)" tag) so the operator knows it is not a
real summary.

### D2 — Daily cost is daemon-persisted

The daemon **accumulates per-day cost to disk** (e.g.
`~/.trusty-mpm/cost/daily.json`, atomic write) so the value **survives TUI and
session restarts**, and exposes it via `GET /api/v1/cost/daily` (§6.2). Rollover &
reset semantics:

- **Day boundary:** the **local** calendar day (host local time). The persisted
  record keys cost by local `YYYY-MM-DD`.
- **Rollover:** the first cost event after the local day changes starts a fresh
  accumulator for the new date; prior days remain on disk (history) but the
  statusline shows **only today's** total.
- **Reset:** there is no manual reset in v1; "today" naturally resets at the local
  midnight rollover. (A future `tm cost reset` is out of scope.)

### D3 — Input + termination via the managed API

Operator messages, raw Esc forwarding, and adapter termination sequences are all
delivered by `POST /api/v1/sessions/managed/{id}/send` (direct tmux injection).
Esc forwarding sends the **raw `\x1b`** byte the same way (§5.5). **Managed-session
prerequisite:** only daemon-managed sessions accept injection (§6.4).

### D4 — Fixed `user` palace

The banner confirms memory by checking a **fixed user-level palace named `user`**
(user/config scope — **shared across projects**, distinct from the per-project
palace and from the SM `session-manager` palace, DOC-14 §8). Naming & location:

- **Name:** the literal palace id `user`.
- **Location:** the user-level trusty-memory store (the same registry
  `palace_create` / `resolve_palace` use, `crates/trusty-common/src/memory_core/`,
  `crates/trusty-memory/src/tools/helpers.rs:340`); palaces are name-keyed, so
  `user` is a stable, user-scoped namespace independent of the current working
  directory.
- **"Active" determination:** memory is "active" when (a) the trusty-memory health
  probe succeeds **and** (b) the `user` palace **exists** (or is created
  idempotently via `palace_create("user")` on first run). If the probe succeeds but
  the palace is absent and creation fails, the banner shows `○ unreachable` with the
  reason. trusty-search "active" is its plain health probe.

---

## 8. Architecture & data sources

### 8.1 Where it lives & how it launches

- **Module:** evolve `crates/trusty-mpm/src/tui/coordinator/` in place; add the
  interactive surfaces (banner/probes, statusline, transcript, filter view,
  multi-line input) as focused submodules under it (respect the 500-SLOC cap —
  split per concept: `banner`, `statusline`, `transcript`, `filter`, `input`).
- **Entry point:** `tm sessions tui` (alias/feature-compatible with the existing
  coordinator entry). Reuse `resolve_daemon_url` for `--url` and `--interval-ms`
  for cadence.
- **Feature flag:** stays behind the existing `tui` feature.

### 8.2 Data feeds

| Need | Feed |
|---|---|
| Session list + status + `last_summary` + `summarizing` + tokens + model | `GET /api/v1/coordinator/context` (enriched, §6.2) |
| Lifecycle (create/stop/resume/kill) | `POST /api/v1/sessions/managed`, `…/{id}/runtime-stop`, `…/{id}/resume`, `…/{id}/decommission` |
| Input / Esc / termination injection | `POST /api/v1/sessions/managed/{id}/send` (D3) |
| Daily cost | `GET /api/v1/cost/daily` (D2) |
| Memory/search probes | trusty-memory health + `palace_create("user")`; trusty-search health |

### 8.3 Daemon URL convention

Always resolve via `resolve_daemon_url` (explicit `--url`/`TRUSTY_MPM_URL` →
`~/.trusty-mpm/daemon.lock` → default) and self-heal on a failed poll (port
`rediscover_daemon`). Track the port-mismatch hazard **#1268**; the TUI never
hard-codes a port.

---

## 9. Non-goals / future

- **EventStream / push refresh** — the live-refresh follow-up (#1278). v1 is
  timer-poll + immediate re-poll-after-mutation; a daemon SSE/websocket push so the
  list/transcript update on activity instead of on a timer is **future**.
- Editing code/files inside a session (the session's own Claude Code owns that).
- A web/graphical UI (`trusty-console` / `trusty-mpm-gui`).
- Implementing the daemon-side inference/summary layer (DOC-14 owns it; this spec
  consumes its `last_summary` / `summarizing` contract).
- Fixing #1268 / #1269 (external dependencies consumed, not solved here).
- A manual daily-cost reset / cost history UI (D2: midnight rollover only in v1).

---

## 10. EPIC & work-item breakdown

> **EPIC #1272 — Interactive sessions TUI (`tm sessions tui`).** Evolve the
> read-only coordinator dashboard into the Claude-Code-style interactive surface:
> backplane banner, numbered scrollable list with blinking summarizing bullets,
> cost statusline, growing summary transcripts with interleaved user input,
> filter-to-session + raw-Esc forwarding, per-adapter termination sequences, and an
> expandable multi-line input — all over the managed API. This spec is the
> authoritative contract the children implement against.

| ID | Title | Summary | Depends-on | Issue | Net-new? |
|----|-------|---------|------------|-------|----------|
| **STUI-0** | Startup banner + backplane probes | Banner (name + `CARGO_PKG_VERSION`), memory (`user` palace, D4) + search active probes, active-session count + `/help` line (§3.1). | — | **NEW** (under #1272) | **Net-new** |
| **STUI-1** | Numbered scrollable list layout | Numbered rows (≥5 visible), project id/name, selection/scroll, ordinals for inline addressing (§3.2/§3.6). | STUI-0 | #1272 / #1278 (scroll) | Partly net-new (numbering, ≥5 reserve) |
| **STUI-2** | Statusline (costs + daily cost) | model · tokens up/down · session cost · daily cost; daily cost via `GET /api/v1/cost/daily` (§3.4, D2). | STUI-1 | **NEW** (+ daemon D2) | **Net-new** |
| **STUI-3** | Status bullets + blinking summarizing | Glyph-by-status; **blinking** bullet while `summarizing`; row shows number + id (§3.3, D1). | STUI-1 | **NEW** (consumes #1275) | **Net-new (blink)** |
| **STUI-4** | Daemon `last_summary` + `summarizing` consumption | Wire the enriched coordinator-context fields (`last_summary`, `summarizing`, tokens, model) into TUI state (§6.2). | STUI-1 | **#1275** (daemon `last_summary`) | Consumes daemon work |
| **STUI-5** | Summary transcript + user-input rendering | Track last-summarized line; append new bullet on change; exact format `<n>.<id>: <summary>`; user input below the bullet in distinctive style (§4). | STUI-4 | **NEW** (under #1272) | **Net-new** |
| **STUI-6** | Inline addressing `/<n> <msg>` | Parse `/<digits> <text>`; deliver via managed send (D3); append user line; disambiguate from slash commands (§5.2). | STUI-5 | **#1276** (slash framework) | **Net-new** |
| **STUI-7** | Filter view + Esc forwarding + lifecycle + termination sequence | Filter-to-session transcript; raw-Esc forward + `q`/double-Esc exit; create/terminate with active-session confirm; per-adapter termination sequence (mpm = `/mpm-session-pause`) (§5.4–5.6, §6.3). | STUI-6 | **#1277** (lifecycle cmds) | **Net-new** |
| **STUI-8** | Live refresh + immediate re-poll | Timer poll + re-poll-after-mutation; PageUp/Dn + mouse-wheel scroll for list & transcript (§3.6). | STUI-1 | **#1278** (live refresh/scroll) | Consumes #1278 |
| **STUI-9** | Hardening + SLOC split + tests | Panic-safe terminal restore; split submodules under the 500-SLOC cap; unit tests for line-builders, summary-change detection, `/<n>` parse, Esc/filter state machine, banner probes (§8.1). | STUI-0…8 | **#1279** (hardening) | Consumes #1279 |
| **STUI-10** | Expandable multi-line input box | Multi-line composer that grows then scrolls; `Shift/Alt+Enter` newline; history recall on empty (§3.5). | STUI-1 | **NEW** (under #1272) | **Net-new** |

**Net-new scope** (no existing child issue — file under #1272): banner + probes
(STUI-0), statusline cost/daily-cost (STUI-2, + daemon D2 endpoint), transcript
state + summary bullets + user-input rendering (STUI-5), blinking summaries
(STUI-3 blink), filter view + Esc forwarding + adapter termination sequence
(STUI-7), multi-line input (STUI-10). **Daemon prerequisites** to land first:
`last_summary` + `summarizing` (#1275), the daily-cost accumulator/endpoint (D2),
the per-adapter termination-sequence trait method (§6.3), and token/model
surfacing for the statusline.

---

## 11. Behavior contract (summary)

- **Inputs:** keystrokes; enriched `coordinator/context` polls (`last_summary`,
  `summarizing`, tokens, model); `cost/daily`; managed-session lifecycle/send
  responses; trusty-memory/`user`-palace + trusty-search health.
- **Outputs:** a rendered full-screen TUI (banner → list/transcript + statusline +
  multi-line input); `POST`s to managed send + lifecycle endpoints; a restored
  terminal on exit (always, even on panic).
- **Preconditions:** the `tm` daemon is reachable or becomes reachable
  (unreachable state is rendered, not fatal); injection targets are **managed**
  sessions (D3).
- **Postconditions:** fleet mutations and sent messages reflect in the
  list/transcript within one refresh; a new summary bullet appears iff
  `last_summary` changed; the terminal is restored on quit.
- **Error conditions:** unreachable daemon → notice + URL re-resolution;
  unconfigured inference → marked fallback summary (D1); non-managed injection →
  "not managed" notice; terminate of an active session → confirmation gate.

---

## 12. References

- DOC-13 Coordinator TUI: `docs/specs/tui-coordinator.md`; modules
  `crates/trusty-mpm/src/tui/coordinator/`.
- DOC-14 Session Manager (SM) Agent: `docs/specs/session-manager-agent.md`
  (inference, providers, summaries §3.6/§5.4, `session-manager` palace §8).
- Managed session API: `crates/trusty-mpm/src/daemon/managed_routes/`
  (`mod.rs`, `lifecycle.rs`); `crates/trusty-mpm/src/session_manager/`.
- Coordinator endpoints: `crates/trusty-mpm/src/daemon/coordinator.rs`,
  `.../daemon/api/coordinator_routes.rs`;
  `crates/trusty-mpm/src/client/http_client/types.rs` (`CoordinatorSession`,
  `CoordinatorContext`).
- URL resolution: `crates/trusty-mpm/src/core/discovery.rs`.
- Memory palace engine: `crates/trusty-common/src/memory_core/`
  (`registry.rs` `create_palace`); `crates/trusty-memory/src/tools/helpers.rs:340`
  (`resolve_palace`).
- Skill: `session-manager-driver` (raw-pane inference, spawn→observe→answer→stop).
- Issues: **#1272** (SM TUI epic), **#1275** (daemon `last_summary`), **#1276**
  (slash framework), **#1277** (lifecycle cmds), **#1278** (live refresh/scroll),
  **#1279** (hardening), **#1268** (daemon URL vs bind port), **#1269**
  (trust-dialog / headless-spawn blocker).

---

## 13. Change log

- **2026-06-17** — Initial draft (DOC-16, `SPEC-SM-TUI-01~draft`). Authoritative
  spec for the interactive `tm sessions tui`: banner + backplane probes, numbered
  scrollable list with blinking summarizing bullets, cost/daily-cost statusline,
  growing summary transcript with interleaved user input (exact format
  `<n>.<id>: <summary>`), inline addressing `/<n> <msg>`, filter-to-session with
  raw-Esc forwarding and `q`/double-Esc exit, create/terminate with active-session
  confirmation and per-adapter termination sequence (mpm = `/mpm-session-pause`),
  and an expandable multi-line input. Baked in decisions D1–D4 (daemon-side
  inference + `summarizing` signal; daemon-persisted daily cost; managed-API
  delivery; fixed `user` palace). Work-items STUI-0…STUI-10 mapped to epic #1272
  and children #1275–#1279.
