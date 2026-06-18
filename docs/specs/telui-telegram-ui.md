# DOC-19 — TELUI: the Telegram UI for trusty-mpm (`t_sess_bot`)

**Status:** Draft
**Subsystem:** trusty-mpm — control surface / Telegram
**Owner:** Engineering (trusty-mpm)
**Last-updated:** 2026-06-17
**Spec ID:** `SPEC-TELUI-01~draft` (DOC-19)
**Builds on:** DOC-16 — Interactive Sessions TUI (`docs/specs/sessions-tui-interactive.md`,
PR #1387 / EPIC #1272) — the **authoritative feature set TELUI mirrors**; DOC-14 —
Session Manager (SM) Agent (`docs/specs/session-manager-agent.md`); DOC-18 — Metacoding
(`docs/specs/metacoding-vision.md`, the three ONB-3 control surfaces).
**Cross-ref:** the existing Telegram module (`crates/trusty-mpm/src/telegram/`, teloxide 0.13),
the managed session API (`crates/trusty-mpm/src/daemon/managed_routes/`), the coordinator-context
endpoints (`crates/trusty-mpm/src/daemon/coordinator.rs`,
`.../daemon/api/coordinator_routes.rs`), and issues **#1272** (SM TUI epic — feature parent),
**#1275** (daemon `last_summary` / `summarizing` — DONE), the cost-daily endpoint (STUI-2 / **#1399**),
and the per-adapter termination sequence (STUI-7 / **#1402**).

> **Scope note.** This is a **behavior + UX requirements** spec for **TELUI** — the
> Telegram control surface for `trusty-mpm`, exposed by the `t_sess_bot` bot. TELUI must
> deliver **all** the interactive sessions-TUI (DOC-16 / "STUI") capabilities, rendered and
> controlled through **Telegram UI primitives** instead of a terminal. It is a
> **presentation/control layer** over the **same daemon backend the TUI uses** — it does
> **not** re-implement the session-manager engine, the inference layer, or the lifecycle
> logic. TELUI is a **peer** of the TUI (and the future Slack surface) as the metacoding
> control surfaces (DOC-18 §ONB-3). This spec specifies what the bot must do and which
> daemon contracts it consumes; it does **not** implement the bot. The PR that carries this
> doc opens **no** Rust changes.

---

## 1. Current state (audited)

The Telegram module lives at `crates/trusty-mpm/src/telegram/` behind the `telegram`
feature (`teloxide = "0.13"`, features `["macros"]`; `crates/trusty-mpm/Cargo.toml`). It is
**single-operator** (`BotOptions::allowed_user_id`, `is_authorized`, `telegram/mod.rs:119`)
and the bot is `t_sess_bot`, its token resolved from `TELEGRAM_BOT_TOKEN` in `.env.local`
(`resolve_token` / `read_dotenv_key`, `telegram/mod.rs:78`).

### 1.1 What already works (reuse, do not rebuild)

| Capability | Where | Notes |
|---|---|---|
| Plain text send | `telegram/mod.rs::on_message` | reply via `bot.send_message` |
| **HTML parse mode** | `telegram/mod.rs:254,265` | `ParseMode::Html` on replies |
| **`set_my_commands`** (19 commands) | `telegram/mod.rs:179`, `telegram/commands.rs` | `/sessions /status /send /kill /approve /deny /pair /start /connect /doctor /help /projects /discover /adopt /config /snapshot /subs /overseer /tmux` — `bot_command_descriptions_fit_telegram_limits` asserts the count is **19** |
| **Inline keyboards** | `telegram/formatter/mod.rs:221` | session/project/tmux lists attach `InlineKeyboardButton::callback` rows |
| **Callback queries** | `telegram/mod.rs::on_callback` (`:345`) | parses `verb:arg`; verbs `status:` `approve:` `deny:` `adopt:` `setproj:`; answers the query to clear the client spinner |
| **64-byte callback guard** | `telegram/formatter/mod.rs:354` (`fits_callback`), `short_id` (`:384`) | truncates ids so `verb:arg` stays ≤ 64 bytes (`short_id_truncates_long_ids`) |
| Authorization gate | `telegram/mod.rs::is_authorized` (`:119`) | rejects non-allowed user ids on messages **and** callbacks (`telegram/mod.rs:355`) |
| Alert loop | `telegram/alerts.rs`, `telegram/mod.rs::run_alert_loop` (`:423`) | polls `GET /sessions` + per-session `events/poll`, pushes permission/memory/overseer alerts |

### 1.2 What is NOT yet used (net-new TELUI scope)

TELUI requires these Telegram primitives and daemon feeds that the current module does
**not** use (audited: `grep` for each returns **no hits** under `telegram/`):

- **`editMessageText`** — live in-place updates (no live list / no rotating glyph today).
- **Pinned messages** (`pinChatMessage`) — no persistent statusline.
- **Typing indicators** (`sendChatAction`) — no "working" affordance.
- **Focused-session state** — no per-chat notion of a currently addressed session.
- **Pagination** — inline keyboards render one flat list.
- **The enriched coordinator-context + managed API** — the module still polls the **legacy
  `/sessions`** API (`telegram/mod.rs:473,484`) and has no `last_summary` / `summarizing`
  consumption, no `GET /api/v1/coordinator/context`, and no `POST /api/v1/sessions/managed*`.

Cited files: `telegram/mod.rs`, `telegram/commands.rs`, `telegram/formatter/mod.rs`,
`telegram/alerts.rs`.

---

## 2. STUI → Telegram affordance mapping

TELUI maps each DOC-16 (STUI) feature onto a Telegram primitive. There is **no terminal**,
so spatial/animation affordances (blink, multi-pane layout) are re-expressed as edits,
emoji, and pushed messages.

| STUI feature (DOC-16) | Telegram primitive in TELUI |
|---|---|
| Numbered scrollable session list (§3.2) | **Paginated inline keyboard** — N rows/page + `‹ Prev` / `Next ›` buttons (callback `page:<n>`) |
| Select / filter to a session (arrow+Enter, §5.4) | Callback **`focus:<id>`** → enter **focused-session view**; bot remembers the focused session per chat |
| Status glyphs `●◌◍○` (§3.3) | Emoji **`🟢` active / `⏳` summarizing / `🔔` awaiting-approval / `⚪` idle** prefixing each row |
| Blinking "summarizing" bullet (§3.3) | Telegram has **no blink** → a **rotating glyph** (`⏳→🔄→⏳…`) cycled by periodic **`editMessageText`** while `summarizing` is true |
| Growing summary transcript (§4) | Summary bullets **pushed as new messages** on each `last_summary` change (one message per new bullet) |
| Statusline: model · tokens · session cost · daily cost (§3.4) | A **single pinned message**, **edited in place** (`editMessageText`) each refresh |
| Operator input → session (§5.2, §5.4) | Chat text messages routed to the managed **`/send`** endpoint (focused or `/<n>`-addressed) |
| Inline addressing `/<n> <msg>` (§5.2) | Native chat message matching `^/(\d+)\s+(.+)$` → managed send to ordinal `<n>` |
| Slash commands `/help`, `/new`, terminate (§5.6) | **Bot commands** (`set_my_commands`) + inline confirm keyboards |
| Esc / stop forwarding (§5.5) | **`[⏹ Stop (Esc)]`** button → callback `esc:<id>` → managed send of raw `\x1b` |
| Termination sequence (§5.6, §6.3) | **`[⏸ Pause]`** button → callback `pause:<id>` → managed send of the adapter sequence (mpm = `/mpm-session-pause`) |
| Create / terminate with active warning (§5.6) | **Two-step inline confirm** (`[Confirm] [Cancel]`) before any destructive action |
| Startup banner + backplane probes (§3.1) | **`/start` welcome** message: tool + version + memory/search probe lines + active-session count |
| Expandable multi-line input (§3.5) | **Native** — Telegram messages are already multi-line (non-issue) |

### 2.1 Telegram HARD CONSTRAINTS (NORMATIVE — every work item must respect)

- **64-byte callback data.** All `verb:arg` callback payloads must fit in 64 bytes — reuse
  the existing `fits_callback` / `short_id` guard (`formatter/mod.rs:354,384`).
- **≤ 8 buttons per inline-keyboard row** (and a sane total per message) — pagination is
  mandatory, not optional, for the list.
- **`editMessageText` rate limit (~20 edits/min per chat).** The rotating-glyph and pinned
  statusline edits **share** this budget; TELUI must throttle (coalesce edits, cap the
  glyph cadence, e.g. one edit per 3–5 s) and **back off** on HTTP 429.
- **4096-char message cap.** Summary bullets and any long body must truncate with an
  ellipsis before send.
- **5 s typing action.** `sendChatAction(typing)` expires after ~5 s and must be re-sent
  for longer operations.
- **Long-polling.** TELUI runs in long-poll mode (current `teloxide` dispatcher); **webhook
  mode is a non-goal** (§5).

---

## 3. Shared daemon backend

**Principle: the daemon is the single source of truth.** TELUI holds only presentation
state (focused session, last-rendered summary line per session, pagination offset, pinned
message id). All fleet state, summaries, costs, and lifecycle live in the daemon — exactly
the feeds DOC-16 §8.2 lists for the TUI. TELUI and the TUI are interchangeable clients of
the **same** endpoints.

| Need | Endpoint (same as the TUI) |
|---|---|
| Session list + status + `last_summary` + `summarizing` + tokens + model | `GET /api/v1/coordinator/context` (enriched, #1275 fields) |
| Lifecycle (create/stop/resume/kill) | `POST /api/v1/sessions/managed`, `…/{id}/runtime-stop`, `…/{id}/resume`, `…/{id}/decommission` |
| Input / Esc / termination injection | `POST /api/v1/sessions/managed/{id}/send` (direct tmux injection) |
| Per-session activity / answer | `…/{id}/activity`, `…/{id}/answer` |
| Daily cost | `GET /api/v1/cost/daily` (D2) |
| Health / backplane probes | `GET /health` (+ trusty-memory / trusty-search health for the `/start` banner) |

**Prerequisites** (mirrors DOC-16 §6 / §10):

- **#1275 — daemon `last_summary` + `summarizing`** on the coordinator-context session
  object. **Status: DONE.** Gates TELUI-2/3/4/5.
- **Cost-daily endpoint** `GET /api/v1/cost/daily` (DOC-16 D2) — **STUI-2 / #1399**, in
  flight. Gates TELUI-2's daily-cost field; until live, render `today —`.
- **Per-adapter termination sequence** (DOC-16 §6.3) — **STUI-7 / #1402**, in flight. Until
  the daemon applies the adapter sequence on the terminate path, the bot **MAY hard-code
  `/mpm-session-pause`** for the `[⏸ Pause]` button (mpm adapter only), migrating to the
  adapter-driven path when #1402 lands.

**Migration note.** TELUI replaces the legacy `/sessions` polling (`telegram/mod.rs:473`)
with the enriched `GET /api/v1/coordinator/context` + managed API; the alert loop
(`telegram/alerts.rs`) is rewired onto the same managed feed (TELUI-1).

---

## 4. EPIC & work-item breakdown (TELUI-0 … TELUI-11)

> **Feature parent: EPIC #1272** (interactive sessions surface). TELUI is the **Telegram
> rendering** of that epic; each TELUI-N mirrors the like-numbered STUI item. The TELUI-N
> ids below are **authoritative and shared with the tracking tickets.**

| ID | Title | Mirrors STUI | Daemon deps | New TG capability? |
|----|-------|--------------|-------------|--------------------|
| **TELUI-0** | `/start` welcome + backplane probes | STUI-0 | `/health`, memory + search health | **n** |
| **TELUI-1** | Migrate list → coordinator-context + managed API + **paginated inline keyboard** | STUI-1 | `GET /api/v1/coordinator/context`, managed API | **y** (pagination callback `page:<n>`) |
| **TELUI-2** | **Pinned statusline** message: model · tokens · session+daily cost | STUI-2 | #1275 fields + `GET /api/v1/cost/daily` (#1399) | **y** (pin + `editMessageText`) |
| **TELUI-3** | Status glyphs + **rotating "summarizing"** indicator | STUI-3 | #1275 (`summarizing`) | **y** (`editMessageText`) |
| **TELUI-4** | Coordinator-context polling + **summary change-detection** state | STUI-4 | #1275 (`last_summary`) | **n** (state only) |
| **TELUI-5** | **Summary transcript push** on `last_summary` change | STUI-5 | #1275 (`last_summary`) | **n** (push messages) |
| **TELUI-6** | **Focused-session state** + free-text → managed `/send` | STUI-7 (filter) | `POST …/{id}/send` | **y** (focus state) |
| **TELUI-7** | **Stop (Esc)** + **Pause** buttons | STUI-7 | `POST …/{id}/send`; adapter seq (#1402) | **y** (verbs `esc:` `pause:`) |
| **TELUI-8** | **Two-step destructive confirm** for stop/kill | STUI-7 | `…/{id}/runtime-stop`, `…/{id}/decommission` | **y** (`editMessageText`) |
| **TELUI-9** | **`/new` guided conversation** to spawn managed sessions | STUI-7 (create) | `POST /api/v1/sessions/managed` | **y** (conversation state) |
| **TELUI-10** | **Live list editing** via `editMessageText` + immediate re-poll after mutations | STUI-8 | coordinator-context | **y** (`editMessageText`) |
| **TELUI-11** | **Hardening**: auth on all callbacks, 64-byte guards, rate-limit backoff, unknown-session callbacks, tests | STUI-9 | — | **n** |

### 4.1 Per-item contracts (summary)

- **TELUI-0 — `/start` welcome.** On `/start`, reply with a welcome message naming
  `trusty-mpm sessions v<CARGO_PKG_VERSION>`, memory + search probe lines (✅/⚠️), and the
  active-session count + a `/help` hint. Unreachable backplane → ⚠️ line, never a hard
  error. (Mirrors STUI-0 banner; no new TG capability.)
- **TELUI-1 — paginated list.** Replace the legacy `/sessions` list with rows built from
  `GET /api/v1/coordinator/context`; render N rows/page (respect the 8-button row cap) with
  `‹ Prev` / `Next ›` (`page:<n>`); each row carries `focus:<id>`. Re-key the alert loop
  onto the same feed. **Gates TELUI-6/7/8/9.**
- **TELUI-2 — pinned statusline.** Send once, **pin**, then `editMessageText` it each
  refresh: `model <id> · ↑<up> ↓<down> · session $<x> · today $<y>`. Daily cost from
  `/api/v1/cost/daily` (#1399); unknown fields render `—`; respect the edit-rate budget.
- **TELUI-3 — glyphs + rotating summarizing.** Map status → `🟢/⏳/🔔/⚪`; while
  `summarizing` is true, **rotate** the glyph via throttled `editMessageText`; clear on the
  first poll where `summarizing` is false. Missing field (older daemon) → never rotate.
- **TELUI-4 — change-detection state.** Per session, track the last-rendered `last_summary`;
  detect change on each poll (identical values do **not** re-fire). Pure presentation state,
  no new TG capability. **Gates TELUI-5.**
- **TELUI-5 — transcript push.** On a detected `last_summary` change, **push a new message**
  formatted `<n>.<id>: <summary>` (DOC-16 §4.3 exact format), truncated to 4096 chars.
  Operator messages render as ordinary chat (Telegram already interleaves them by time).
- **TELUI-6 — focused session.** A `focus:<id>` callback sets the chat's focused session;
  subsequent **plain chat text** routes to that session via `POST …/{id}/send`; `/<n> <msg>`
  still addresses by ordinal regardless of focus. A header/banner reflects the focused id.
- **TELUI-7 — Stop / Pause buttons.** The focused-session view shows `[⏹ Stop (Esc)]`
  (callback `esc:<id>` → send raw `\x1b`) and `[⏸ Pause]` (callback `pause:<id>` → send the
  adapter termination sequence; mpm hard-codes `/mpm-session-pause` until #1402).
- **TELUI-8 — two-step confirm.** `/stop` and `/kill` (and the Pause/decommission path on an
  **active** session) first render `[✅ Confirm] [✖ Cancel]`; Confirm edits the message to a
  result via `editMessageText`; Cancel aborts. No destructive call without confirm.
- **TELUI-9 — `/new` guided spawn.** A short **conversation** (repo/path → git ref → task →
  runtime → optional name) collects fields, then `POST /api/v1/sessions/managed`; spawn
  errors (incl. trust-dialog) surface inline; on success, re-poll + offer `focus:<id>`.
- **TELUI-10 — live editing.** Edit the list/statusline messages in place via
  `editMessageText` on each refresh **and** immediately re-poll after any mutation
  (send/spawn/stop/kill) so the view reflects the change without waiting for the timer.
- **TELUI-11 — hardening.** Authorize **every** callback (reuse `is_authorized`); enforce
  64-byte callback guards; back off on 429 (edit-rate); handle **unknown/stale-session**
  callbacks gracefully (answer the query with a notice, no panic); unit tests for row
  builders, pagination math, summary change-detection, `/<n>` parse, focus routing, confirm
  state machine, and callback authorization.

### 4.2 Dependency diagram

```
                         #1275 (DONE: last_summary + summarizing)
                                       │
TELUI-0 ──► TELUI-1 ──► TELUI-4 ──┬──► TELUI-2  (also needs cost-daily #1399)
 (start)    (list +    (poll +    ├──► TELUI-3  (rotating glyph)
            paginate)  change-    └──► TELUI-5  (transcript push)
              │        detect)
              │
              └──► TELUI-6 ──► TELUI-7  (esc:/pause: — pause seq #1402)
               (focus +    └─► TELUI-8  (two-step confirm)
                send)      └─► TELUI-9  (/new guided spawn)

TELUI-10 (live editMessageText + re-poll) integrates across 1/2/3/5/8.
TELUI-11 (hardening + tests) lands last, over TELUI-0…10.
```

- **TELUI-4 gates 2, 3, 5** (they consume the polled `summarizing` / `last_summary` state).
- **TELUI-1 gates 6 → 7 / 8 / 9** (focus + lifecycle need the managed-API list).
- **TELUI-10** integrates the live-edit + re-poll behavior across the rendered surfaces.
- **TELUI-11** is the final hardening + test pass over everything.

---

## 5. Non-goals / future

- **Multi-user.** TELUI stays **single-operator** via `allowed_user_id` (`telegram/mod.rs`).
  Multi-tenant / per-user fleets are out of scope.
- **Webhook mode.** TELUI uses **long-polling** (the current `teloxide` dispatcher);
  webhook-based delivery is future.
- **Slack peer surface.** The DOC-18 §ONB-3 Slack control surface is a **separate**,
  post-MVP deliverable; TELUI does not implement it.
- **Re-implementing the engine.** Inference/summaries (DOC-14, #1275), lifecycle, and cost
  accounting live in the daemon — TELUI consumes them, never reproduces them.
- **A graphical/web UI.** That is `trusty-console` / `trusty-mpm-gui`, not TELUI.

---

## 6. Change log

- **2026-06-17** — Initial draft (DOC-19, `SPEC-TELUI-01~draft`). TELUI is the Telegram
  rendering of the DOC-16 interactive sessions surface, a peer of the TUI and the future
  Slack surface (DOC-18 §ONB-3) over the **same daemon backend**. Audited the existing
  `t_sess_bot` teloxide module (HTML send, 19 commands, inline keyboards, `verb:arg`
  callbacks with a 64-byte guard) and named the net-new primitives it must add
  (`editMessageText`, pinned messages, typing indicators, focused state, pagination,
  managed/coordinator-context API). Mapped every STUI affordance to a Telegram primitive
  with the platform hard-constraints, listed the shared daemon endpoints + prereqs (#1275
  DONE; cost-daily #1399; termination sequence #1402), and broke the work into TELUI-0…
  TELUI-11 with a dependency diagram.

---

## 7. References

- DOC-16 Interactive Sessions TUI: `docs/specs/sessions-tui-interactive.md` (EPIC #1272;
  STUI-0…STUI-10 — the feature set TELUI mirrors).
- DOC-18 Metacoding: `docs/specs/metacoding-vision.md` (§ONB-3 — Telegram / Slack / TUI
  control surfaces).
- DOC-14 Session Manager (SM) Agent: `docs/specs/session-manager-agent.md` (inference,
  summaries, `session-manager` palace).
- Telegram module (current state): `crates/trusty-mpm/src/telegram/` —
  `mod.rs` (dispatch, `on_callback`, alert loop, `is_authorized`, `resolve_token`),
  `commands.rs` (19-command enum), `formatter/mod.rs` (inline keyboards, `fits_callback`,
  `short_id`), `alerts.rs` (alert loop on legacy `/sessions`).
- Managed session API: `crates/trusty-mpm/src/daemon/managed_routes/`.
- Coordinator endpoints: `crates/trusty-mpm/src/daemon/coordinator.rs`,
  `.../daemon/api/coordinator_routes.rs`.
- Issues: **#1272** (SM TUI epic / feature parent), **#1275** (daemon `last_summary` +
  `summarizing`, DONE), **#1399** (cost-daily endpoint, STUI-2), **#1402** (per-adapter
  termination sequence, STUI-7).
</content>
</invoke>
