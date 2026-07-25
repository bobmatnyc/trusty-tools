# DOC-54 — Trusty Agents Product Specification

**Status:** Draft  
**Subsystem:** trusty-agents — product vision / agent model / eventstream processing  
**Owner:** Engineering (trusty-agents) / Bob Matsuoka  
**Last-updated:** 2026-07-24  
**Spec ID:** `SPEC-AGENTS-01~draft` … `SPEC-AGENTS-08~draft` (DOC-54)

---

## 1. Executive Summary

Trusty Agents is a personal-productivity agent platform focused on three things: **tasks** (user-facing workstreams), **workstreams** (resumable, agent-tagged memory history), and **eventstream processing** (the primary capability). The product is NOT a general coding/project-work agent — that domain belongs to Co-work. Instead, Trusty Agents connects assistants to event sources (Gmail, Google Calendar, Slack, etc.), delivers events as they arrive, asks how to respond, learns preferences in trusty-memory, and adapts over time. All agents are declarative-only, instantiated from templates into local configuration packages, never coded.

---

## 2. SPEC-AGENTS-01 — Product Vision: Event → Ask → Learn → Adapt {#SPEC-AGENTS-01~draft}

### 2.1 Core loop (Bob decision, 2026-07-24)

Trusty Agents implements a closed learning loop:

1. **Event** — an upstream provider (Gmail, Google Calendar, Slack, etc.) emits an event (message received, event created, etc.).
2. **Ask** — the agent's listener surfaces the event; a bound assistant asks the user how to respond (or acts autonomously if learned to do so).
3. **Learn** — the user's response, instructions, or approvals are recorded in trusty-memory.
4. **Adapt** — the assistant's behavior adapts based on learned preferences, progressively earning autonomy for approved patterns (the ask-first / learn-to-act permission model).

### 2.2 Scope: NOT project work

Trusty Agents is scoped explicitly to personal productivity and event-driven tasks. **Project work (code review, development coordination, deployment) remains in Co-work's domain.** If a user needs to manage software projects, they use Co-work; if they need event-driven personal assistance (respond to urgent emails, schedule meetings, answer calendar conflicts), they use Trusty Agents.

### 2.3 Eventstream processing is primary

Of the three dimensions (tasks, workstreams, eventstream), eventstream processing is the highest priority. The product is built around the expectation that most agent activations come from upstream events, not user-initiated chat.

---

## 3. SPEC-AGENTS-02 — Agent Model: Declarative-Only, Template-Based {#SPEC-AGENTS-02~draft}

### 3.1 Agents are 100% declarative (standing rule, Bob decision 2026-07-16, reaffirmed 2026-07-24)

An agent is exactly two kinds of content, never a third:

1. **Instructions** — Markdown prose defining behavior, persona, and policy. No executable semantics; the LLM reads it as a system prompt.
2. **A manifest** — YAML declaring which platform-hosted primitives this agent is bound to (stores, tools, listeners). The manifest is data: names, references, and scalars/lists — never a code path, a shell command, or a local script.

**No coded agents exist.** If the semantics needed exceed declarative instructions + manifest data, the feature scope is out-of-bounds for Trusty Agents.

### 3.2 NO subagents — only agent templates

The product **drops the subagent concept entirely**. Agents are instantiated from **templates** only. The first and currently only template is `assistant`.

New-agent creation (GUI flow, Concierge configurator, CLI) instantiates a template into a local directory package:

```
agents/<name>/
  agent.toml          # manifest: stores, tools, listeners
  persona.md          # main instructions + frontmatter
  events/             # per-connector event-specific instructions
    gmail.md
    calendar.md
    slack.md
```

### 3.3 Agent definitions are LOCAL CONFIGURATION, never committed to git

Agent packages live in the user's local agent directory (e.g., `~/.trusty-agents/agents/`), **not** in the repository. They are user-specific, account-specific, and often multi-tenant (different agents for different email identities, calendars, etc.). A `backup-on-change` convention stores dated snapshots under a local backups directory (`~/.trusty-agents/backups/`); agents are never version-controlled as repository artifacts.

---

## 4. SPEC-AGENTS-03 — Demo Roster: Three Agents {#SPEC-AGENTS-03~draft}

### 4.1 Izzie — Personal assistant (eventstream-connected)

- **Role:** Handles personal events, email, calendar.
- **Bound to:** User's personal Gmail account, personal Google Calendar.
- **Listeners:** `gmail-personal`, `calendar-personal`.
- **Typical actions:** Compose email, schedule meetings, manage event responses.
- **Created by:** Onboarding flow (#3790), user-named "Personal Assistant" (or custom name).

### 4.2 CTO Assistant — Work assistant (code/analysis focused)

- **Role:** Handles work analysis, code review context, technical decisions.
- **Bound to:** Duetto work email account (via gworkspace multi-tenant identity #3795).
- **Listeners:** `gmail-work` (if event-connected; not required for MVP).
- **Typical actions:** Search work mail, fetch meeting notes, analyze codebase context.
- **Created by:** User add-agent flow (from template).

### 4.3 Concierge — Fixed, system role (harness configurator, no derivations)

- **Role:** Control-plane assistant, harness configurator. Full tool access. Runs Opus via OpenRouter (exception to the all-Sonnet default; Bob 2026-07-24).
- **Alias:** The ctrl agent — aliased "Concierge" for user familiarity.
- **Special:** No derivations allowed. No user may create a second "Concierge" or extend it. This role is reserved.
- **Configuration:** Fully configurable by prompting it (its own persona is custodian of agent configuration, tool provisioning, etc.).

---

## 5. SPEC-AGENTS-04 — Agent Configuration: The Config Triple {#SPEC-AGENTS-04~draft}

> **SUPERSEDED (2026-07-25) by [DOC-57 — Five-Section Agent Configuration](./agent-config-five-sections.md) (`SPEC-AGENTCFG-01~draft` … `-09~draft`).**
> The owner redefined the configuration model on 2026-07-25: *"Instead of tools, let's show skills. Should be Personality, Knowledge (list knowledge tools including and MCP connections to knowledge stores), Skills (each tool should be wrapped in a skill), Listeners, the Permissions."*
> The three legs below are retained in substance and redistributed across five sections — Stores widens into **Knowledge** (DOC-57 §4), Tools is replaced by **Skills** (DOC-57 §5), Listeners is unchanged (DOC-57 §6), and **Personality** and **Permissions** are promoted to first-class sections (DOC-57 §3, §7).
> This section is kept for historical grounding: the `[[stores]]` / `[tools]` / `[[listeners]]` config surfaces it describes all keep working unchanged under DOC-57 §9's compatibility contract, and existing code comments referencing "the config triple" remain accurate as history (DOC-57 §9.2).
> **§8.4 below is likewise superseded** by DOC-57 §8.2's tab mapping.

Each agent defines exactly three binding kinds, persisted in `agent.toml` and the `persona.md` manifest:

| Leg | Answers | Config table | Example |
|---|---|---|---|
| **Stores** | What the agent *knows* (OKG knowledge trees / search indexes) | `[stores]` | `allow = ["izzie-personal-kb"]` |
| **Tools** | How the agent *acts* (MCP tool allow-list) | `[tools]` | `allow = ["compose_email", "manage_events"]` |
| **Listeners** | What the agent *reacts to* (inbound event bindings, further-filtered) | `[[listeners]]` | `name = "gmail-personal"`, `event_types = ["message.received"]` |

### 5.1 Stores (Knowledge): One per agent

Each agent has exactly **ONE** dedicated OKG store (Bob decision, 2026-07-24: "stores are one per agent; we can allow agents to talk to each other").

- **Contents:** The agent's OKF knowledge tree (structured markdown KG) + search index over that tree.
- **Cross-agent knowledge:** Flows via agent-to-agent communication (via MCP tools or chat), NOT via shared stores.
- **Configuration:** `agent.toml` lists the store name; the harness initializes/attaches it on agent startup.

### 5.2 Tools (Actions): MCP allow-list

Each agent declares which MCP tools it is allowed to call, following the existing trusty-agents tool-registry security posture. The allow-list acts as a capability boundary; the LLM cannot invoke tools not in the list.

- **Example tools:** `compose_email`, `manage_events`, `search_gmail_messages`, `create_md_knowledge_graph` (the knowledge-builder tool, per onboarding requirements #3790).
- **Concierge exception:** Concierge has full tool access (no allow-list restrictions).
- **Configuration:** `agent.toml` `[tools]` table, `allow` list.

### 5.3 Listeners (Reactions): Inbound event bindings

Listeners are **NOT** MCP tools — they are inbound API connections to upstream event providers. An agent **acts** on the world via MCP tools; an agent **reacts** to the world via listeners.

- **Direction:** Listener delivers events from provider → harness event bus; the harness runtime then routes events to bound agents.
- **Transport:** Direct Rust API calls into connector layers (e.g., `trusty-gworkspace`'s `api::services::gmail::*`), not MCP JSON-RPC.
- **Not MCP:** Listeners are never registered as tool schemas and never invoked by the model.
- **Filtering (two stages):**
  1. **Listener-level filter** — narrows what the listener pulls from the provider (e.g., Gmail `labelIds: ["INBOX"]`). Defined once per listener in `config.toml`.
  2. **Per-agent-binding filter** — further narrows which events reach a *specific* agent (event type, sender pattern, calendar, label exclusions). Defined in `agent.toml` `[[listeners]]` entries. Two agents can bind the same listener with different filters.
- **Configuration:** `agent.toml` `[[listeners]]` entries, each naming a listener defined in `config.toml` and specifying agent-specific filter overrides.

---

## 6. SPEC-AGENTS-05 — Persona and Event-Specific Instructions {#SPEC-AGENTS-05~draft}

### 6.1 Main instructions + per-connector instructions

Each agent has:

- **`agents/<name>/persona.md`** — Main instructions file. Markdown prose + YAML frontmatter (agent metadata, persona attributes, policy).
- **`agents/<name>/events/<connector>.md`** — Per-connector event-specific instructions. When an event arrives via a specific connector (e.g., Gmail), the harness loads the corresponding file into context before handling the event.

### 6.2 Event-specific instruction loading

When an event of type `gmail.message_received` arrives via the Gmail listener, the harness:

1. Loads the agent's main `persona.md` instructions into context.
2. Loads `agents/<name>/events/gmail.md` (if present) into context.
3. Surfaces the event data (message body, sender, labels, etc.) to the model.
4. The model reasons over the event using both sets of instructions.

This allows assistants to adapt behavior to different event sources without duplicating the main persona logic.

### 6.3 Connector naming convention

Connectors are named after the upstream provider or channel type: `gmail`, `calendar`, `slack`, `telegram`, etc. An agent can have instructions for any subset of the connectors its listeners bind.

---

## 7. SPEC-AGENTS-06 — Eventstream Listeners: Gmail & Google Calendar {#SPEC-AGENTS-06~draft}

### 7.1 Gmail: Pub/Sub pull or history.list polling

**Primary:** Cloud Pub/Sub pull-subscription (if a GCP project + topic exist).  
**Fallback:** `users.history.list` polling with `historyId` cursor (zero GCP setup).

#### 7.1.1 Pub/Sub pull (primary, viable local-first)

- **Setup:** GCP project with Cloud Pub/Sub topic and pull subscription; grant `roles/pubsub.publisher` to `gmail-api-push@system.gserviceaccount.com` (org policies may block; an ops/config concern per tenant).
- **Call:** `users.watch(topicName=...)` stays live — re-arm every ~6 days (expiration is 7 days; safety margin).
- **Harness action:** Periodically pull the subscription (configurable interval, e.g., 20–30s) instead of exposing a public HTTPS callback.
- **Payload:** Notifications carry only `historyId`; harness calls `history.list(startHistoryId=...)` to fetch actual message/thread changes.
- **Quota:** 1 notification/sec per watched user; excess is dropped.

#### 7.1.2 history.list polling fallback

- **Setup:** Zero additional GCP configuration. Uses existing gworkspace OAuth scopes.
- **Harness action:** Persist `historyId` cursor; poll `history.list(startHistoryId=...)` at configurable interval (e.g., 2–5 min, quota-conscious).
- **Failure mode:** On `410 GONE` (cursor invalid — e.g., after ~1 week gap or too many changes), drop cursor, re-baseline.
- **Latency:** Bounded by poll interval, higher quota burn per check.

#### 7.1.3 Scope

Existing gworkspace OAuth scopes (`oauth-crates/trusty-gworkspace/src/api/constants.rs`) already grant access for both paths. No new scope needed.

### 7.2 Google Calendar: events.list polling with syncToken (only viable transport)

**Push via webhook (NOT viable local-first):** Requires valid SSL certificate, public HTTPS endpoint, domain verification, and manual renewal before expiration (no auto-renew). Poor fit for a local-first harness.

**Polling via syncToken (standard, viable):**

- **Setup:** Zero additional configuration beyond existing Calendar API access.
- **First poll:** Omit `syncToken`; receive `nextSyncToken` and full event list.
- **Subsequent polls:** Pass `nextSyncToken` from previous response; receive only changed/deleted events since last poll.
- **Cheap:** Small response bodies on steady state (incremental sync).
- **Per-calendar:** Distinct listener/cursor (or at least distinct cursor) needed per watched `calendarId`.
- **Failure mode:** On `410 GONE` (token invalidated — e.g., after ~1 week gap or too many changes), drop cursor, re-baseline.
- **Scope:** Existing `https://www.googleapis.com/auth/calendar` already covers `events.list`.

### 7.3 Polling engine (shared for both Gmail history-poll and Calendar syncToken)

#### 7.3.1 Per-listener poll interval

Configurable per `config.toml` `[[listeners]]` entry (`poll_interval_secs`). Sane defaults:

- Gmail Pub/Sub-pull: ~20–30s (cheap empty pulls).
- Gmail history-poll fallback: ~2–5 min (quota-conscious).
- Calendar syncToken: ~5 min (tune per calendar activity).

#### 7.3.2 Cursor persistence

One cursor per listener instance, persisted alongside gworkspace token storage (keyed by named identity per #3795 multi-tenant work):

- Gmail → last-seen `historyId`.
- Calendar → `nextSyncToken` per `calendarId`.

#### 7.3.3 Dedup

Providers redeliver: Pub/Sub is at-least-once, resync can replay already-seen items. Listener emits a stable idempotent event ID (provider message/event ID). Short-TTL dedup cache + downstream event-bus dedup (#3800) drops repeats before reaching an agent.

#### 7.3.4 Backoff and renewal

- **Transient errors (429/5xx):** Exponential backoff with jitter.
- **410 GONE (invalid cursor):** Immediate full resync (drop cursor, re-baseline) — not a retry loop.
- **Gmail watch renewal:** Re-arm on a schedule well inside the 7-day expiration (independent of poll interval; pull-mode still requires the topic subscription to stay live via `watch`).

#### 7.3.5 Shared connector code

Both polling engine and Pub/Sub-pull call `trusty-gworkspace`'s `api::services::gmail::*` / `api::services::calendar::*` functions **directly as library calls** — the same functions `trusty-gworkspace-mcp`'s tool handlers call today. New connector-layer methods needed:

- `users.watch`, Pub/Sub pull-subscription read (Gmail).
- `history.list` (Gmail).
- `events.list` with `syncToken` (Calendar).

These are new alongside existing tool-facing methods, not a fork of the client/auth layer.

### 7.4 Two-stage filtering (deterministic ingestion → user-driven agent wakes)

**Stage one (listener-level, deterministic ingestion):**
- Listener-level filters in `config.toml` bound what is ingested from the provider at all (quota/noise control).
- Ingestion is deterministic: every event matching a listener's stage-one filter is fetched, deduplicated, and placed on the harness event bus.
- Events past stage one are **visible in the Events pane** (tagged by listener name) regardless of whether any agent is bound to them or whether agent-specific filters would apply.

**Stage two (per-agent-binding, agent wake trigger):**
- Per-agent-binding filters in `agent.toml` `[[listeners]]` entries decide which events WAKE each agent.
- Only events passing both stage-one (ingestion) AND stage-two (agent filter) result in an agent reaction.
- When an event passes stage two, the agent receives a reaction trigger; the agent responds in its own chat pane (see §8.3–8.4).

**Inference spent only on wakes:** Ingestion and filtering (stages one and two) are fully deterministic; inference is spent only on agent reactions and on guiding filter configuration (e.g., suggesting filters based on past reactions and learned preferences).

### 7.5 Config shape (sketch)

**`config.toml` (harness-level listener definitions, stage-one filters):**

```toml
[[listeners]]
name = "gmail-personal"
connector = "gmail"
identity = "izzie"                 # gworkspace named identity/profile (#3795)
transport = "pubsub-pull"          # "pubsub-pull" | "history-poll" (auto fallback)
gcp_project = "izzie"
pubsub_topic = "projects/izzie/topics/gmail-personal-events"
poll_interval_secs = 30
filter = { label_ids = ["INBOX"], label_filter_behavior = "INCLUDE" }

[[listeners]]
name = "calendar-personal"
connector = "google-calendar"
identity = "izzie"
transport = "poll-synctoken"
calendar_id = "primary"
poll_interval_secs = 300
```

**`agent.toml` (per-agent listener bindings, stage-two filters):**

```toml
[stores]
allow = ["izzie-personal-kb"]

[tools]
allow = ["compose_email", "manage_events", "search_gmail_messages"]

[[listeners]]
name = "gmail-personal"
event_types = ["message.received"]
filter = { from = ["*@family.com"], exclude_labels = ["PROMOTIONS"] }

[[listeners]]
name = "calendar-personal"
event_types = ["event.created", "event.updated"]
```

---

## 8. SPEC-AGENTS-07 — GUI Structure: Chat | Events Navigation, Tasks Sidebar {#SPEC-AGENTS-07~draft}

### 8.1 Navigation: Chat | Events only

The top-level navigation reduces to two primary panes:

- **Chat** — Agent conversation (main interaction).
- **Events** — Incoming-events view with filtering controls.

Removed: Projects, Settings, Personality tabs (out of scope for MVP).

### 8.2 Left sidebar: Tasks (user-facing) = Workstreams (internal)

**User-facing terminology:** TASKS  
**Internal/technical term:** Workstreams

Users see "Tasks" in the sidebar as a **filter/view over the single continuous agent conversation**, not as separate contexts.

**Sidebar structure:**

- Lists inferred workstream classifications (agent-inferred task/topic boundaries within the conversation).
- Grouped/collapsible by agent.
- Shows workstream status (date, summary, latest activity).
- Clicking a workstream **filters and highlights** its corresponding conversation segment within the one continuous chat (does not load a separate context).
- All agent memories remain accessible; the sidebar organizes, not isolates.

### 8.3 Chat pane title and agent selector

- **Pane title:** Shows the currently active agent's display name (e.g., "Izzie", "CTO Assistant", "Concierge").
- **Agent selector:** In-pane selector (dropdown or button) to switch active agents.
- **Location:** Chat pane header (top-right or near title).

### 8.4 In-pane agent configuration

- **Trigger:** Gear icon in the chat pane header (top-right).
- **Opens:** In-pane config form. **Superseded by [DOC-57](./agent-config-five-sections.md) §8.2**, which fixes the five sections, their labels and their order (Personality → Knowledge → Skills → Listeners → Permissions). The historical list below said "four sections" and enumerated five; DOC-57 §8.2 is the authority.
  1. **Personality** — View/edit persona instructions.
  2. **OKG Store** — Manage agent's knowledge base. *(→ DOC-57 "Knowledge")*
  3. **Tools** — View/manage MCP tool allow-list. *(→ DOC-57 "Skills")*
  4. **Listeners** — Configure event bindings and filters.
  5. **Permissions** — Manage agent's autonomy level (learned patterns). *(→ DOC-57 §7, widened beyond autonomy)*

### 8.5 Add-agent flow

- **UI action:** "+ Add Agent" button or link (e.g., in a control panel or settings area).
- **Flow:** Instantiates the `assistant` template into a new local directory package.
- **Steps:**
  1. User provides agent name.
  2. System creates `agents/<name>/` with template files (agent.toml, persona.md, events/ directory).
  3. User can customize agent immediately (set persona, bind listeners, etc.).

### 8.6 Events pane: Ingestion view + user-driven agent wakes

**Display and data source:**
- Shows events ingested by listeners (stage-one filtering, §7.4) — everything past the listener-level filter is visible here, tagged by listener name (Gmail, Calendar, Slack, etc.).
- Events are displayed regardless of whether any agent is bound or whether an agent's stage-two filter would apply; the pane shows the full ingestion palette.

**Filtering controls (stage-two, agent wake trigger):**
- User can include/exclude event types per agent (e.g., "wake Izzie on `gmail.message.received` from family").
- User can set cost guidance (e.g., "High: X events/day" warning if too many event types are enabled).
- Excluded events remain visible so users can re-enable them; the pane shows all ingested events, not just enabled ones.

**Event→Chat pane connection:**
- When an event passes both stage-one (ingestion) AND stage-two (user include filter for a specific agent), that agent is **awakened** and produces a **reaction in its own chat pane**.
- The agent's reaction is surfaced as a proposal/question (ask-first autonomy default) unless the pattern has previously earned autonomy.
- The reaction is automatically classified into an inferred task/workstream (§9.2) within the agent's continuous conversation (§9.1).

**Inference scope:**
- Ingestion (stage one) and filtering (both stages) are fully deterministic; no inference is spent here.
- Inference is spent only on agent reactions to passed-events and on suggesting filter configurations based on learned preferences.

**UI details:** Exact display format (list vs. cards, sorting, pagination) is implementation scope; the information architecture above governs the spec.

---

## 9. SPEC-AGENTS-08 — Memory and Workstreams: Classification Over Continuous Conversation {#SPEC-AGENTS-08~draft}

### 9.1 Single continuous conversation per agent (aligned with tcode infinite-sessions model)

Each agent maintains **ONE continuous conversation per session** — not separate chat contexts. All agent memories are accessible at all times; there is no context walling or clearing. This aligns with the established implicit-workstream-inference and infinite-sessions decisions from tcode (2026-07-19/20).

**Terminology:**
- **User-facing:** Tasks (sidebar, user language).
- **Internal:** Workstreams (memory tagging, APIs, spec vocabulary).

### 9.2 Workstreams as classifications and filters (not context boundaries)

Workstreams are **agent-inferred classifications over the continuous conversation**, persisted in trusty-memory:

- **Agent behavior:** The agent infers task/topic boundaries from conversation content and explicitly classifies segments as workstreams (e.g., "This looks like an email-response task").
- **Memory tagging:** Conversation rows are tagged with the inferred workstream ID; memory rows for the same workstream are grouped in queries.
- **Sidebar filter:** The Tasks sidebar lists inferred workstreams and acts as a **filter/view over the one continuous chat + activity**, not as separate context containers.
- **User filter:** Users can filter the sidebar to show tasks matching certain criteria (agent, date, topic, status) — this reorders and highlights the conversation but does not split it.

### 9.3 "+ New Task" and "Clear Context" semantics

- **"+ New Task" button:** An explicit **classification hint / topic boundary marker**, not a context wipe. Signals to the agent "the following is a new task" — the agent uses this to infer a workstream boundary and tag subsequent conversation rows accordingly.
- **All memories accessible:** The agent retains full access to prior conversation and learned preferences — "+ New Task" does not clear context.
- **"Clear Context" removed:** This concept does not exist in the model. Context is never cleared; classification organizes the one continuous conversation, it never walls off information.

### 9.4 Learning in the continuous conversation

Preferences, learned patterns, and approvals are recorded in trusty-memory inline with the continuous conversation:

- **User approvals:** "Yes, always respond to Bob's emails like this" → recorded as a learned pattern within that workstream's history.
- **Corrections:** "Actually, don't send that — do this instead" → recorded as a preference override, tagged with the workstream.
- **Cross-workstream learning:** Patterns learned in one workstream are accessible to the agent in all subsequent workstreams (no context walling).
- **Autonomy progression:** Initial interactions are ask-first; repeated approvals of the same pattern gradually earn autonomy for that pattern (the ask-first / learn-to-act permission model).

### 9.5 Persistence and backup

The continuous conversation is persisted in trusty-memory (local database or backend, per trusty-mpm's session/persistence model). Workstream classifications are metadata within that conversation history, enabling reconstruction of task-scoped views. Agent configuration packages (`agents/<name>/`) are backed up locally (dated snapshots in `~/.trusty-agents/backups/`).

### 9.6 Filterable-Chat Context-Assembly Model (Bob 2026-07-24)

The agent's context (what is sent to the LLM for inference) is dynamically assembled based on workstream classification and user focus. This model combines in-band classification, hierarchical summaries, and focused filtering while preserving the agent's access to all memories on demand.

#### 9.6.1 Classification in-band

Every prompt submitted by the user (or every agent reaction to an event) is sent to the LLM together with the current set of workstream/task classifications (a closed vocabulary of labels + explicit "new: <label>" escape so new workstreams are deliberate, preventing label drift). The LLM returns its response PLUS a suggested classification for that turn. Sidebar Tasks list is the rollup of these labels.

User affordances: rename/merge/re-tag workstreams (corrections are learning signals). Periodic consolidation passes (dream-consolidation shaped) merge near-duplicate workstream labels.

#### 9.6.2 Summaries (global + per-workstream)

Two independent summary layers:

- **Global prompt-history summary:** The existing infinite-context design's global summary (stored, cacheable), always present in context.
- **Per-workstream summary:** Generated every N turns (configurable `summarize_every`) and cached. Reflects turn N to the last summarization point within that workstream's filtered history.

Two independent tuning knobs:
- `summarize_every`: N-turn cadence for per-workstream summary regeneration.
- `recent_window`: Number of raw turns sent in focused mode (after the summaries).

Re-tagging a turn lazily invalidates that workstream's cached summary (regenerated on next focused access).

Reuses trusty-memory's durable record of all prompts/responses + compression machinery (tcode infinite-sessions pattern) — one mechanism, not a parallel one.

#### 9.6.3 Focused mode (context assembly)

Clicking a workstream in the sidebar enters **focused mode** for that workstream:

1. **History filter:** Visible prompt history is filtered to matching prompts (or "All" for unfocused).
2. **UI note:** The chat interface notes "working on task X" (or no note when unfocused/all).
3. **Context assembly (stable order):**
   - Global summary (small, always present — preserves "agent has full memory access", filtering constrains raw-turn history only).
   - Per-workstream summary (if focused; if unfocused, N recent prompts of all types).
   - N recent prompts of the filtered type (matching the workstream or all prompts if unfocused).
   - Memory tools always live (agent can query memory at any time).

4. **Stable assembly order** (for prompt-cache prefix reuse):
   - Global summary → per-WS summary (only in focused mode) → recent prompts.
   - Per-workstream summary only changes at N-turn boundaries, so the cache busts once per summarization cycle, not per turn.

#### 9.6.4 Task bleed (agent agency over classification)

Focused context is a **starting point, not a wall**. The agent has full agency:

- **Memory search:** When a prompt bleeds across task boundaries, the agent can REQUEST A MEMORY SEARCH and pull broader context on demand.
- **Re-classification:** The agent can DECIDE to KEEP or CHANGE the task classification after reviewing the broader context.
- **Gentle nudge (no forcing):** If focused on task A and a prompt classifies as B, respond normally, label honestly with the new classification, and surface a gentle "this looks like a different task" nudge. Never force or drop the turn; the agent's honest classification is authoritative.

---

## 10. Design Decisions Log

| Decision | Date | Tracking Issue |
|----------|------|-----------------|
| Filterable-chat context-assembly: in-band classification + global + per-WS summaries + focused-mode filtering + agent memory-search agency | 2026-07-24 | Bob 2026-07-24, approved filterable-chat design |
| Inference provider policy: Izzie + CTO Assistant = Sonnet via OpenRouter (default); Concierge = Opus via OpenRouter (exception, harness configurator role) | 2026-07-24 | Bob 2026-07-24 morning, verified from resolver logs |
| ONE continuous conversation per agent (not separate chat contexts); workstreams are classifications/filters over that conversation | 2026-07-24 | Coordinator clarification, aligns with tcode #2026-07-19/20 |
| User-facing term "Tasks" = internal "Workstreams"; sidebar is a filter/view, not context boundaries | 2026-07-24 | Coordinator message, 2026-07-24 |
| "+ New Task" is a classification hint/topic boundary, not a context wipe; "Clear Context" removed (concept doesn't exist) | 2026-07-24 | Coordinator clarification |
| NO subagents; use template-based instantiation | 2026-07-24 | #3816 |
| Per-connector event-specific instruction files (`events/<connector>.md`) | 2026-07-24 | #3817 |
| GUI reshape: Chat\|Events nav, workstreams/tasks sidebar, agent selector, in-pane config | 2026-07-24 | #3818 |
| Listeners as inbound API bindings, NOT MCP tools; two-stage filtering (listener-level + per-agent) | 2026-07-24 | #3820 |
| Gmail: Pub/Sub pull + history.list fallback; Calendar: syncToken polling | 2026-07-24 | #3820 |
| Eventstream processing is primary product focus | 2026-07-24 | #3798 |
| One OKG store per agent; cross-agent knowledge via agent-to-agent communication | 2026-07-24 | 3820 (config triple discussion) |
| Demo three agents: Izzie (personal), CTO Assistant (work), Concierge (fixed/ctrl) | 2026-07-24 | #3818, #3816 |
| Declarative-only agents (no coded agents) | 2026-07-16 | #2791, reaffirmed 2026-07-24 |

---

## 11. Open Questions (Undecided as of 2026-07-24)

1. **Event → workstream attribution:** When an event arrives and triggers a conversation, should the system automatically tag the resulting workstream with the event type/listener? Or should users manually associate events with workstreams? (Mechanics beyond simple include/exclude filtering.)

2. **Agent-to-agent communication protocol:** Agents learn to talk to each other (cross-agent knowledge sharing). How is this modeled? Direct message passing, shared memory queries, or agent-to-agent tool calls? Not yet designed.

3. **Autonomy model details:** The ask-first / learn-to-act pattern is directional (approvals earn autonomy), but the exact decision-tree (which patterns earn how much autonomy, how to represent learned exceptions, how to revoke autonomy) is not yet specified.

4. **Multi-provider event correlation:** If a Gmail event and a Calendar event are related (e.g., a meeting scheduling email followed by a calendar-event-created), should the harness correlate them into a single workstream or keep them separate?

5. **Listener restart/renewal policy:** When the harness restarts, should listeners resume from persisted cursors (potentially missing events that arrived during downtime) or re-baseline and resync full? The policy may differ by listener type and agent binding.

---

## 12. References

**Related Issues:**
- #3816 — Declarative agent templates (template instantiation, replaces subagents)
- #3817 — Per-connector event-specific instructions
- #3818 — GUI reshape (Chat|Events nav, workstreams, agent selector)
- #3820 — Listeners spec (Gmail/Calendar transport, two-stage filtering)
- #3798 — Eventstream epic (core product focus)
- #3799 — Event-stream connector framework (generic abstraction)
- #3800 — Event delivery + routing (bus-side durability)
- #3790 — Personal Assistant onboarding epic
- #3795 — Multi-tenancy foundations (gworkspace named identities)
- #2791 — Declarative-only agents (standing rule)

**Related Specs:**
- DOC-41 (SPEC-AGENTFW-01~draft …) — Eve-Style Agent Framework (agent definition format, runtime)
- DOC-47 (SPEC-EVTING-01~draft …) — External Event Ingestion (webhooks & connector push)
- DOC-48 (SPEC-WS-01~draft …) — tcode Workstreams (durable work aggregation)
- DOC-38 (SPEC-SLD-01~draft …) — Spec-Linked Documentation standard

---

## 13. Change Log

- **2026-07-24** — Initial spec (DOC-54, SPEC-AGENTS-01~draft … 08~draft), consolidating Bob's product vision decisions from 2026-07-24 demo and morning directives. Eight sections covering product vision, agent model, config triple, persona/instructions, listeners (Gmail/Calendar detail), GUI structure, memory/workstreams, and open questions. Demo roster (Izzie, CTO Assistant, Concierge) established. User-facing terminology (Tasks = Workstreams) recorded.
