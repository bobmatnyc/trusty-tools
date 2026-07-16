# Agent Gallery Validation — OpenClaw / Hermes vs. trusty-agents Instructions-Only Design

**Date:** 2026-07-16
**Author:** Research pass, trusty-agents track
**Refs:** #2791 (SPEC-AGENTFW epic), PR #2792 (spec branch `spec-eve-style-agents`)
**Spec version validated against:** `docs/specs/trusty-agents-eve-style-agents-spec.md` on
`origin/spec-eve-style-agents` @ `526ff201` — this is the **v2** rewrite (normative,
per-section `SPEC-AGENTFW-01..06`), not the v1 draft PR #2792 originally merged
and rejected by Bob ("the eve research is NOT a spec"). No v3 was present on the
branch at read time; if one lands later this document's primitive-gap findings
should be re-checked against it, particularly §9's channel non-goal (see
Verdict).

## Purpose

Bob's directive: research the most popular **non-coding** agents in the
OpenClaw and Hermes ecosystems (the two frameworks trusty-agents competes
with), then stress-test the "declarative-only, no user code, ever" design
decided 2026-07-16 by asking, for each real-world agent: can trusty-agents
express this today, does it need a new/extended platform primitive, or is it
a genuine limitation of instructions-only composition? The output is both a
validation verdict and a deduplicated backlog (events/connectors/skills/tools)
for TA-M4/TA-M5.

---

## Phase 1 — Research Findings

### OpenClaw

OpenClaw (`github.com/openclaw/openclaw`) is a self-hosted, model-agnostic
personal-AI-assistant framework — originally published November 2025 as
**Clawdbot** by Peter Steinberger, renamed OpenClaw in January 2026, and
since one of the fastest-growing open-source projects (reportedly 250k+
GitHub stars within ~60 days of the rename) [OpenClaw GitHub][oc-gh]
[OpenClaw site][oc-site]. It answers on whichever chat surface the user
already lives in — WhatsApp, Telegram, Slack, Discord, Google Chat, Signal,
iMessage, IRC, Microsoft Teams, Matrix, and a dozen more — voice on
macOS/iOS/Android, and a controllable live Canvas [OpenClaw site][oc-site].
Agents are defined as `SOUL.md` persona files (convention-over-configuration,
similar in spirit to Eve's `instructions.md`), with tool/skill access layered
on via ~50 platform integrations.

The most substantial **gallery** of concrete non-coding agents is the
community-maintained **`mergisi/awesome-openclaw-agents`** repo: 205
production-ready `SOUL.md` templates across 24 categories, each shippable as
a Dockerfile + docker-compose + bot + README, also available machine-readable
as `agents.json` [awesome-openclaw-agents][oc-gallery]. Category sizes (for
scale/popularity signal): Marketing & Content (21), Development (17, coding —
excluded per scope), Business (12), Creative (11), DevOps (10), Finance (10),
Education (8), Personal (7), Productivity (7), Healthcare (7), HR (7), Data
(7), Security (6), Legal (6), E-Commerce (6), Automation (6), SaaS (5), Real
Estate (5), Compliance (4), Freelance (3), Moltbook (3), Supply Chain (3),
Voice (3), Customer Success (2). Position-1 (first-listed) agents per
category and agents cross-linked from the project's own homepage as "featured
deployment" demos are the strongest available popularity signals, since the
repo does not publish per-template star/usage counts.

Ranked list of the 10 OpenClaw agents selected for the validation matrix
(ranked by category prominence + featured/first-listed status):

1. **Orion** (Productivity #1) — task coordination / project management
2. **Compass** (Business #2, homepage-featured) — support ticket triage
3. **Echo** (Marketing #1, homepage-featured) — blog/social content drafting
4. **Pipeline** (Business #3, homepage-featured) — sales lead scoring/outreach
5. **Inbox** (Productivity #4) — email triage and daily digest
6. **Atlas** (Personal #1) — daily schedule optimization
7. **Morning Briefing** (Automation #3) — email/calendar/news daily rollup
8. **Home Automation** (Personal #4) — smart-home control via Telegram
9. **Personal CRM** (Business #6) — contact tracking, follow-up reminders
10. **Meeting Scheduler** (Business #8) — timezone-aware scheduling

### Hermes (disambiguated)

"Hermes" in Bob's OpenClaw-competitive framing resolves to **Hermes Agent**,
released February 2026 by **Nous Research**
(`github.com/NousResearch/hermes-agent`) — described in coverage as "the
first real competitor to OpenClaw," with a stronger emphasis on
self-improving skills, model choice, remote execution, and a built-in
OpenClaw migration path [Hermes coverage][herm-cov]. (Ruled out: "Hermes" as
a Greek-mythology-named unrelated crypto/logistics product and "Hermes" as
the LLM fine-tune family from Nous Research's earlier releases — the agent
framework is the one in the OpenClaw competitive set.) It connects Telegram,
Discord, Slack, WhatsApp, Signal, and CLI from one gateway process
[hermes-agent.org][herm-org].

**Important ecosystem-size finding, stated explicitly per the research
brief's instruction:** Hermes Agent has **no curated persona/template gallery
comparable to OpenClaw's 205-agent repo.** Its own site, its GitHub README,
and third-party writeups were fetched directly and none list named example
agents with triggers/channels/integrations in the way OpenClaw's gallery
does [hermes-agent.org][herm-org] [NousResearch/hermes-agent][herm-gh]
[agentskillshub][herm-hub] [hermesagent.agency][herm-agency]. Hermes's design
philosophy is explicitly **emergent, not templated**: skills are not shipped
as a browsable catalog but are *self-generated at runtime* — "auto-generates
structured skill documents after complex tasks. No manual curation — the
agent captures what worked and stores it for reuse," compatible with the
`agentskills.io` open standard and stored under `~/.hermes/skills/`
[herm-gh]. Broadening scope per the research brief's instruction, the four
concrete Hermes-side data points used in the validation matrix are:

11. **Scheduled Automations** (built-in cron capability, generic) — "daily
    reports, nightly backups, weekly audits, morning briefings," defined in
    natural language, delivered to any connected platform [herm-gh]
    [herm-org].
12. **Competitor-analysis workflow** (self-generated skill, documented
    example) — web research + comparison-table compilation, delivered over
    Telegram, with the skill document auto-created (no custom code)
    [herm-hub].
13. **`openclaw-migration` skill** — an agent-guided, interactive migration
    tool with dry-run previews, itself packaged as a skill under
    `~/.hermes/skills/openclaw-imports/` [herm-gh].
14. **Team & Enterprise (Slack/Discord) assistant** — the general "Team &
    Enterprise" use-case category: collaborative assistant embedded in
    Slack/Discord for team workflows [hermesagent.agency][herm-agency].

14 agents total across both ecosystems (10 OpenClaw + 4 Hermes), within the
requested 10–15 range; Hermes's contribution is smaller and structurally
different by design, not under-researched — this asymmetry is itself a
finding (see Verdict).

---

## Phase 2 — Validation Matrix

Every manifest sketch below binds only to primitives named in
`SPEC-AGENTFW-01..06` (v2) plus two primitives proposed by this validation
pass that **do not exist in any form in the v2 spec** — `events:` (external
trigger binding) and `channels:` (delivery-surface binding) — flagged
inline every time they appear, since classifying against them is the crux of
this exercise.

### 1. Orion — task coordination / PM (OpenClaw, Productivity #1)

```yaml
name: orion
model: anthropic/claude-sonnet-4.5          # SPEC-AGENTFW-06 resolve_model chain
description: "Daily task coordination and project alignment for a small team."

events:                                      # PRIMITIVE-GAP
  schedule:
    cron: "0 8 * * 1-5"                      # weekday morning priorities pass

channels:                                    # PRIMITIVE-GAP
  - telegram
  - slack

tools:
  allowed: [mcp_task_add, mcp_task_list, mcp_task_complete, memory_recall]

memory:
  segment: brief
  top_k: 5

subagents:
  allowed: []
```

*Instructions summary:* Orion opens with a persona describing itself as a
lightweight PM: on each scheduled run it recalls open tasks and deadlines
from the `brief` memory segment, drafts a short prioritized list, and posts
it to the team's Slack/Telegram channel; ad-hoc messages ("what's blocking
X?") are answered from the same memory recall.

*Classification:* **PRIMITIVE-GAP.** The reasoning/persona/tool-binding core
is fully expressible today (`tools.allowed` against `mcp_task_*` — these are
literally the existing `trusty-memory` MCP tools already in this
environment's tool list — plus `memory.segment: brief`, both per
SPEC-AGENTFW-01/06). What's missing is the wake mechanism: a cron-schedule
event trigger and a channel-delivery binding, neither of which the v2 spec
defines (§9 explicitly makes multi-channel a non-goal/open question; no
schedule/trigger concept exists anywhere in `events.rs`, which is
run-lifecycle-only).

### 2. Compass — support ticket triage (OpenClaw, Business #2, featured)

```yaml
name: compass
model: anthropic/claude-haiku-4.5
description: "Triage inbound support tickets, draft first responses, escalate when needed."

events:                                      # PRIMITIVE-GAP
  webhook:
    path: /hooks/support-ticket-created       # e.g. Zendesk/Intercom webhook

channels:                                    # PRIMITIVE-GAP
  - slack
  - discord

tools:
  allowed: [mcp_ticket_fetch, mcp_ticket_reply, mcp_ticket_escalate]  # NEW connector, see inventory

subagents:
  allowed: [escalation-agent]                 # SPEC-AGENTFW-01 subagents.allowed

memory:
  segment: history
  top_k: 8
```

*Instructions summary:* Compass fires on each inbound support-ticket webhook,
drafts a first response using the customer's history (`history` segment),
and either replies directly or hands off via `delegate_to_agent` to
`escalation-agent` when the ticket matches an escalation rule described in
its own instructions.

*Classification:* **PRIMITIVE-GAP** (webhook trigger) **+ SUPPORTED**
(delegation). The escalation handoff is a clean fit for
SPEC-AGENTFW-04's `HandoffContext`/`subagents.allowed` — this is exactly the
declared-subagent-set + structured-handoff pattern the spec already
normatively defines. The blocking gap is purely the inbound-webhook trigger
and a concrete ticketing-system MCP connector (both new).

### 3. Echo — blog/social content drafting (OpenClaw, Marketing #1, featured)

```yaml
name: echo
model: anthropic/claude-opus-4.5
description: "Draft blog posts, social copy, and email content from a content calendar."

events:                                      # PRIMITIVE-GAP
  schedule:
    cron: "0 6 * * *"                        # daily content-calendar check
  on_message:                                 # PRIMITIVE-GAP
    channel: slack                            # ad-hoc "write me a post about X"

channels:                                    # PRIMITIVE-GAP
  - slack
  - webchat

tools:
  allowed: [mcp_calendar_read, mcp_publish_draft]

memory:
  segment: context
  top_k: 5
```

*Instructions summary:* Echo checks a shared content calendar every morning,
drafts any due piece in-house voice, and posts the draft to Slack for
approval; it also answers ad-hoc drafting requests posted directly in the
channel.

*Classification:* **PRIMITIVE-GAP.** Same two gaps as Orion (schedule +
channel), plus a second trigger shape — `on_message` (reactive to inbound
channel text, not just a timer or webhook) — that is a third distinct event
kind the platform needs, not a variant of the other two.

### 4. Pipeline — sales lead scoring / outreach (OpenClaw, Business #3, featured)

```yaml
name: pipeline
model: anthropic/claude-sonnet-4.5
description: "Score inbound leads, draft outreach, and summarize pipeline health weekly."

events:                                      # PRIMITIVE-GAP
  subscribe:                                  # PRIMITIVE-GAP
    connector: crm-leads
    poll_interval: 15m
  schedule:
    cron: "0 9 * * 1"                        # weekly pipeline report

channels:                                    # PRIMITIVE-GAP
  - slack

tools:
  allowed: [mcp_crm_read, mcp_crm_update, mcp_email_draft]

memory:
  segment: agent_memory
  top_k: 10
```

*Instructions summary:* Pipeline polls the CRM for newly-created leads,
scores each against criteria in its own instructions, drafts an outreach
email, and separately produces a Monday pipeline-health summary from
accumulated `agent_memory`.

*Classification:* **PRIMITIVE-GAP.** Introduces the fourth trigger shape:
polling subscription against an external system with no native webhook
(most CRMs' free/starter tiers lack outbound webhooks) — distinct from the
webhook-driven Compass case.

### 5. Inbox — email triage and daily digest (OpenClaw, Productivity #4)

```yaml
name: inbox
model: anthropic/claude-haiku-4.5
description: "Triage inbound email, draft responses, and produce an end-of-day digest."

events:                                      # PRIMITIVE-GAP
  subscribe:
    connector: gmail
    poll_interval: 5m

channels:                                    # PRIMITIVE-GAP
  - telegram

tools:
  allowed: [mcp_gmail_list, mcp_gmail_draft, mcp_gmail_send]

memory:
  segment: history
  top_k: 5
```

*Instructions summary:* Inbox polls Gmail, classifies and drafts responses
to actionable mail, and sends a compressed evening digest to Telegram.

*Classification:* **PRIMITIVE-GAP, mechanism-wise EXTEND not NEW for the
connector.** The Gmail MCP tool itself already exists as a precedent in this
very environment (`mcp__claude_ai_Gmail__*`) — so the *credential-brokered
MCP tool* mechanism (SPEC-AGENTFW-03 §4.2 item 3, `credential_ref`) is the
right, already-specified fit; the gap is exclusively the polling-subscription
event trigger and the Telegram channel, not the Gmail connector concept
itself.

### 6. Atlas — daily planner (OpenClaw, Personal #1)

```yaml
name: atlas
model: anthropic/claude-haiku-4.5
description: "Optimize the day's schedule each morning and evening."

events:                                      # PRIMITIVE-GAP
  schedule:
    cron: "0 7,21 * * *"                     # morning plan + evening review

channels:                                    # PRIMITIVE-GAP
  - telegram

tools:
  allowed: [mcp_calendar_read, mcp_calendar_update]

memory:
  segment: brief
  top_k: 3
```

*Instructions summary:* Atlas reads the day's calendar twice daily,
identifies conflicts/gaps, proposes a reordered schedule, and asks for
confirmation before writing changes back.

*Classification:* **PRIMITIVE-GAP, connector mechanism already precedented.**
The Google Calendar MCP tool exists in this very environment
(`mcp__claude_ai_Google_Calendar__*`), so the calendar-read/write tool
binding is a clean fit for existing `tools.allowed` + `credential_ref`
brokering; only the twice-daily schedule trigger and Telegram delivery are
missing.

### 7. Morning Briefing (OpenClaw, Automation #3)

```yaml
name: morning-briefing
model: anthropic/claude-haiku-4.5
description: "Roll up email, calendar, and news into one daily message."

events:                                      # PRIMITIVE-GAP
  schedule:
    cron: "0 6 * * *"

channels:                                    # PRIMITIVE-GAP
  - telegram
  - whatsapp

tools:
  allowed: [mcp_gmail_list, mcp_calendar_read, mcp_news_fetch]

memory:
  segment: context
  top_k: 3
```

*Instructions summary:* A pure fan-in aggregator: one scheduled run per day,
reads three sources via existing/precedented MCP tool bindings, and composes
a single digest message pushed to two channels.

*Classification:* **PRIMITIVE-GAP** (schedule + multi-channel fan-out — a
useful variant showing `channels:` needs to support delivery to more than
one surface per run, not just a single bound channel).

### 8. Home Automation — smart-home control via Telegram (OpenClaw, Personal #4)

```yaml
name: home-automation
model: anthropic/claude-haiku-4.5
description: "Control smart-home devices from Telegram and react to sensor events."

events:                                      # PRIMITIVE-GAP
  on_message:
    channel: telegram
  subscribe:                                  # PRIMITIVE-GAP — device-state variant
    connector: home-assistant-mqtt
    topics: ["sensor/motion/#", "sensor/door/#"]

channels:                                    # PRIMITIVE-GAP
  - telegram

tools:
  allowed: [mcp_ha_call_service, mcp_ha_get_state]
```

*Instructions summary:* Home Automation responds to Telegram commands
("turn off the lights") and separately reacts to MQTT sensor events
("motion detected at 2am with nobody home → notify") per rules in its own
instructions.

*Classification:* **PRIMITIVE-GAP**, and the one case in this batch that
motivates a *distinct* trigger primitive rather than a reuse of "subscribe":
MQTT/device-state event streams are push-based and topic-filtered, not
poll-interval based like the CRM/Gmail cases — worth a named
`subscribe.protocol: mqtt` variant in the eventual schema rather than
forcing it through the same shape as REST polling.

### 9. Personal CRM (OpenClaw, Business #6)

```yaml
name: personal-crm
model: anthropic/claude-haiku-4.5
description: "Track contacts and surface follow-up reminders."

events:                                      # PRIMITIVE-GAP
  schedule:
    cron: "0 8 * * *"
  on_message:                                  # PRIMITIVE-GAP
    channel: telegram

channels:                                    # PRIMITIVE-GAP
  - telegram

tools:
  allowed: [memory_remember, memory_recall]

memory:
  segment: agent_memory
  top_k: 20
```

*Instructions summary:* Personal CRM is almost entirely a memory-shaped
agent: contacts and interaction notes are stored via `memory_remember` (an
existing trusty-memory MCP tool) and a daily scheduled pass surfaces
follow-ups due today; ad-hoc "who do I owe a reply to?" queries are answered
inline.

*Classification:* **SUPPORTED (core) + PRIMITIVE-GAP (wake/delivery).** This
is the single cleanest existing-primitive fit in the whole batch — the
entire value proposition is `memory.segment: agent_memory` plus the
already-existing `memory_remember`/`memory_recall` MCP tools, exactly as
SPEC-AGENTFW-06 describes. Isolating this one shows the gap in every other
row is structurally the trigger/channel pair, not the reasoning/memory core.

### 10. Meeting Scheduler (OpenClaw, Business #8)

```yaml
name: meeting-scheduler
model: anthropic/claude-haiku-4.5
description: "Coordinate meeting times across timezones from chat requests."

events:                                      # PRIMITIVE-GAP
  on_message:
    channel: slack

channels:                                    # PRIMITIVE-GAP
  - slack

tools:
  allowed: [mcp_calendar_read, mcp_calendar_create_event]
```

*Instructions summary:* Responds to a Slack message like "find 30 min with
Priya next week," reads both calendars, proposes 2–3 slots respecting
timezones, and books on confirmation.

*Classification:* **PRIMITIVE-GAP** (inbound-message trigger + channel;
calendar tool binding itself is precedented/EXTEND, same as Atlas).

### 11. Hermes — Scheduled Automations (generic cron capability)

```yaml
name: nightly-backup-report
model: anthropic/claude-haiku-4.5
description: "Run a nightly backup verification and report results."

events:                                      # PRIMITIVE-GAP
  schedule:
    cron: "0 2 * * *"

channels:                                    # PRIMITIVE-GAP
  - telegram

tools:
  allowed: [mcp_backup_verify]
```

*Instructions summary:* A minimal illustration of Hermes's advertised
"natural-language cron" pattern: one scheduled tool call, one channel
notification of the result.

*Classification:* **PRIMITIVE-GAP.** Directly validates that the schedule
primitive is not a nice-to-have for one or two edge-case agents — it is
Hermes's headline capability, marketed on its own homepage, and it maps to
zero lines of the current trusty-agents spec.

### 12. Hermes — Competitor-analysis workflow (self-generated skill)

```yaml
name: competitor-analysis
model: anthropic/claude-opus-4.5
description: "Research competitors and compile comparison tables on request."

events:                                      # PRIMITIVE-GAP (manual trigger only — least gap-exposed row)
  on_message:
    channel: telegram

channels:                                    # PRIMITIVE-GAP
  - telegram

tools:
  allowed: [web_search, web_fetch]

memory:
  segment: agent_memory
  top_k: 5
```

*Instructions summary:* On request, researches named competitors via web
search/fetch and compiles a comparison table; per Hermes's own framing, a
successful run's method is worth remembering for next time.

*Classification:* **SUPPORTED, with one adapted mechanism.** This is the
best test of whether "no user code, ever" can still capture Hermes's
signature "agent writes its own skill after solving a hard problem" trick
without letting an agent author files. It can: instead of the agent writing
a new `SKILL.md` to disk, the *existing* `memory.segment: agent_memory`
binding (SPEC-AGENTFW-06) already lets a phase write a structured
"how I solved this" record via `memory_remember`, retrievable on a future
run via `memory_recall`. Functionally equivalent, and arguably safer — no
new file lands in the agent's own instructions-only package, so the
never-user-code invariant holds. Only the trigger/channel pair is a gap, not
this mechanism.

### 13. Hermes — `openclaw-migration` skill

```yaml
name: openclaw-migration
model: anthropic/claude-sonnet-4.5
description: "Interactively migrate an OpenClaw SOUL.md persona into a trusty-agents package."

events:
  on_message:                                 # PRIMITIVE-GAP
    channel: cli

tools:
  allowed: [memory_recall]

skills: |
  ## SOUL.md → trusty-agents mapping
  1. Parse SOUL.md frontmatter into `description`/`model`.
  2. Map each referenced channel to a `channels:` entry (flag any channel
     with no trusty-channels connector yet as unsupported, per the inventory
     in this document).
  3. Map each referenced OpenClaw skill/integration to an MCP `tools.allowed`
     entry, prompting for a `credential_ref` per secret found.
  4. Emit a dry-run diff before writing the new agent package.
```

*Instructions summary:* A meta-agent: walks a user through converting an
OpenClaw persona file into a trusty-agents instructions-only package,
dry-run first, purely by asking questions and writing YAML/Markdown — never
generating executable code, which keeps it inside the declarative
invariant even though its job is literally "help someone else define an
agent."

*Classification:* **SUPPORTED.** No new primitive at all — this is exactly
a `skills.md`-style procedure (existing #482 directory-package convention)
combined with the `memory_recall`/existing-tool set. Included specifically
because it is the one gallery row that requires *zero* new primitives,
proving the design isn't gap-everywhere by construction.

### 14. Hermes — Team & Enterprise (Slack/Discord) assistant

```yaml
name: team-assistant
model: anthropic/claude-sonnet-4.5
description: "General-purpose team assistant embedded in Slack/Discord."

events:                                      # PRIMITIVE-GAP
  on_message:
    channel: slack

channels:                                    # PRIMITIVE-GAP
  - slack
  - discord

tools:
  allowed: [mcp_task_add, mcp_task_list, memory_recall]

subagents:
  allowed: [meeting-scheduler, personal-crm]   # SPEC-AGENTFW-01/04
```

*Instructions summary:* A team-facing generalist that answers ad-hoc
requests in Slack/Discord and delegates specialized asks (scheduling,
contact lookups) to narrower subagents via `delegate_to_agent`.

*Classification:* **PRIMITIVE-GAP (trigger/channel) + SUPPORTED
(delegation).** Reinforces #2's finding: multi-agent composition via
`extends`/`subagents.allowed`/`HandoffContext` is the strongest-covered part
of the spec; channel/trigger binding is the weakest.

---

## Aggregated Primitive Inventory (TA-M4/TA-M5 backlog input)

Deduplicated across all 14 rows above, each marked against its current
status in the v2 spec.

| # | Primitive | Status | Notes |
|---|---|---|---|
| 1 | Agent persona/instructions (`system_prompt.content`, frontmatter) | **exists** | SPEC-AGENTFW-01 §2.1; both `.toml` and `.md`+frontmatter loaders. |
| 2 | `extends` (single-parent agent inheritance) | **extend** (spec'd, not yet implemented) | SPEC-AGENTFW-01 §2.2 — NEW field, full merge-rule algorithm already normative. |
| 3 | `tools.allowed`/`tools.deny` (MCP tool binding via `ToolExecutor`) | **exists** (allow) / **extend** (deny) | SPEC-AGENTFW-01 §2.2, SPEC-AGENTFW-03 §4.1 — `ToolExecutor` already unifies native + MCP-external + MCP-management tools today. |
| 4 | `subagents.allowed` + `HandoffContext` (declared-subagent delegation) | **extend** (spec'd, not yet implemented) | SPEC-AGENTFW-01 §2.2, SPEC-AGENTFW-04 §5.2 — the single best-covered composition primitive in this whole exercise (used cleanly by rows 2, 14). |
| 5 | `memory.segment`/`top_k` (declarative memory-scope binding) | **extend** (spec'd, not yet implemented) | SPEC-AGENTFW-06 §7.2 — 5 existing Palace segments (`agent_memory`/`code_index`/`context`/`brief`/`history`), just not yet declarable from the agent's own file. |
| 6 | Model/provider resolution (`resolve_model`/`adapter_for_model`) | **exists** | SPEC-AGENTFW-06 §7.1; per-agent override already opaque provider-qualified string. |
| 7 | Credential-brokered MCP tool (`credential_ref`, stdio env / HTTP header injection) | **extend** (spec'd, secret-store backend undecided — owner-decision item 2) | SPEC-AGENTFW-03 §4.2 item 3 — this is the exact mechanism needed for every named-SaaS connector below (Gmail, Calendar, CRM, GSC, Home Assistant, ticketing). |
| 8 | HTTP-transport MCP client | **extend** (spec'd, not yet implemented) | SPEC-AGENTFW-03 §4.2 item 2 — closes the stdio-only gap; several connectors below (webhook-fed SaaS APIs) will want this. |
| 9 | Checkpoint/resume (phase-level durability) | **extend** (spec'd, not yet implemented) | SPEC-AGENTFW-02 — orthogonal to the gallery agents' triggers, but relevant to reliability of long scheduled/webhook-driven runs. |
| 10 | Internal lifecycle events (`PhaseStarted`/`PhaseDone`/`RunResumed`, SSE) | **exists** | `events.rs`/`events_sse.rs` — **not** the same thing as an external trigger; ~universal source of confusion, called out explicitly in Verdict. |
| 11 | **`events.schedule` (cron trigger)** | **new** | Needed by 8/14 rows (Orion, Echo, Pipeline's weekly report, Atlas, Morning Briefing, Personal CRM, Hermes-cron, and implicitly most others as a fallback poll). Zero prior art in either loader or `events.rs`. **Highest-priority new primitive** — it is Hermes's headline marketed feature and the single most common OpenClaw gallery trigger. |
| 12 | **`events.on_message` (inbound-channel-message trigger)** | **new** | Needed by 6/14 rows (Echo, Home Automation, Personal CRM, Meeting Scheduler, competitor-analysis, migration skill, team-assistant). Distinct from `channels:` (delivery) — this is *reception*. |
| 13 | **`events.webhook` (inbound-HTTP trigger)** | **new** | Needed by Compass and structurally by most Business/DevOps/Finance categories not in this batch (Ledger, Deploy Guardian, Incident Responder, NPS Followup) — flagged for completeness even though only 1/14 rows above uses it directly. |
| 14 | **`events.subscribe` (polling-subscription trigger)** | **new** | Needed by Pipeline (CRM), Inbox (Gmail), and structurally by Rank/Scout/Listing Scout/Brand Monitor from the wider gallery. Needs a `poll_interval` and, per row 8, a distinct push/topic-filtered variant. |
| 15 | **`events.subscribe` protocol variant for device/IoT streams (MQTT-style topic push)** | **new** | Row 8 (Home Automation) shows plain poll-interval subscribe doesn't fit push/topic-filtered sources cleanly — worth a named `protocol: mqtt` (or generic pub/sub) sub-shape rather than overloading the REST-polling shape. |
| 16 | **`channels:` (delivery-surface binding, multi-channel fan-out)** | **new** | Needed by essentially every row (13/14) — and an **explicit non-goal in the v2 spec** (§9, owner-decision item 4: "Should trusty-agents grow its own channel adapters... RESOLVED: pending."). This is the largest single mismatch between the task's framing ("channels via trusty-channels/SM-proxy") and what the current spec branch actually commits to — see Verdict. |
| 17 | **Channel connector implementations** (Telegram/Slack/Discord/WhatsApp adapters) | **new** | The concrete build-out behind #16 — a `trusty-channels` crate (or SM-proxy binding) per channel, analogous to how `McpService` is the concrete build-out behind the `tools:` binding. |
| 18 | **Named SaaS/MCP connectors**: Gmail, Google Calendar, generic CRM, Google Search Console, Home-Assistant/MQTT, ticketing (Zendesk/Intercom-style), Stripe/payments | **extend** | Mechanism (item 7) already covers all of these; what's missing is the concrete `McpService` config entries + short skill docs per service — not new platform code. Gmail and Google Calendar already have a working precedent *in this very session's tool list* (`mcp__claude_ai_Gmail__*`, `mcp__claude_ai_Google_Calendar__*`), which is strong evidence this class of gap is genuinely "extend," not "new." |
| 19 | **Memory-as-skill-promotion convenience** (`memory.auto_inject: true` — automatically surface top-k relevant `agent_memory` recalls into the phase prompt without an explicit tool call) | **new (minor, optional)** | Not required to cover Hermes's self-generated-skill pattern (row 12 shows the existing `memory_remember`/`memory_recall` pair already suffices) but would remove one explicit tool-call step per phase; a nice-to-have, not a blocking gap. |

**Counts:** 4 primitives already **exist** unmodified (#1, #3-allow, #6, #10);
6 are **extend** — spec'd in v2 but not yet implemented, or a concrete
connector build-out on an already-specified mechanism (#2, #3-deny, #4, #5,
#7, #8, #9, #18 — counting #18 as one class); **9 are genuinely new**,
undefined in any form in the v2 spec (#11–#17, #19, plus #16's connector
build-out #17 counted once). The five new primitives with the widest blast
radius across the gallery, in priority order: **`events.schedule`** (cron),
**`channels:`** (delivery binding), **`events.on_message`** (inbound
channel trigger), **`events.subscribe`** (polling), **channel connector
implementations** (the concrete Telegram/Slack/Discord/WhatsApp adapters
behind `channels:`).

---

## Verdict

**Does instructions-only hold?** Yes, with one honest caveat about
completeness, not correctness. Across all 14 agents, nothing required an
agent package to contain executable code, a runtime-authored file, or a
bespoke Rust/Python shim — every single row was expressible as
frontmatter + YAML manifest + Markdown instructions, including the two rows
(12, 13) that most directly tested the "no user code, ever" boundary:
Hermes's self-writing-its-own-skills pattern maps cleanly onto the *already
existing* `memory.segment`/`memory_remember`/`memory_recall` primitives
(SPEC-AGENTFW-06) rather than requiring the agent to author a new file, and
the OpenClaw-migration meta-agent (row 13) is itself just a `skills.md`
procedure. The strongest-covered dimension of the design is **composition**:
`extends` + `subagents.allowed` + `HandoffContext` (SPEC-AGENTFW-01/04) cover
every multi-agent handoff pattern the gallery produced (Compass→escalation,
team-assistant→scheduler/CRM) without any gap.

**The honest limit: none of these 14 agents can actually *run* yet, because
the v2 spec has no concept of what wakes an agent up or where it talks.**
13 of 14 rows landed **PRIMITIVE-GAP** on exactly the same two axes —
trigger (`events:`) and delivery (`channels:`) — and this is not a minor
edge case: `events.schedule` (cron) is literally Hermes's headline marketed
feature, and every single OpenClaw gallery agent's entire value proposition
is "reachable on the channel you already use." The v2 spec's internal
`Event` enum (`PhaseStarted`/`PhaseDone`/`RunResumed`, SSE-streamed) is a
*run-lifecycle observability* mechanism, not an external-trigger mechanism —
conflating the two would be the wrong fix. And §9 of the v2 spec explicitly
makes multi-channel adapters a **non-goal**, with owner-decision item 4
("Should trusty-agents grow its own channel adapters... RESOLVED: pending")
left unresolved. This means the task's framing — "channels (Slack/Telegram
etc. via trusty-channels/SM-proxy)" as an assumed-supported binding — is
**ahead of what the spec branch actually commits to today**; this
validation pass surfaces that gap rather than papering over it. If a v3 spec
lands resolving item 4 in the affirmative (and adding `events.schedule`/
`events.on_message`/`events.webhook`/`events.subscribe` as new top-level
manifest sections alongside the existing `tools:`/`subagents:`/`memory:`),
instructions-only fully holds for the entire researched gallery. Absent
that, instructions-only holds for the *reasoning core* of every agent but
not yet for making any of them reachable in the real world — which is
precisely why Bob's directive was to build out the events/connectors, not to
re-litigate the no-code invariant.

**Classification totals:** SUPPORTED: 2/14 (rows 9, 13 — Personal CRM's core
and the migration meta-agent land cleanly on existing/spec'd primitives with
no blocking gap beyond the universal trigger/channel pair, which for row 13
is CLI-only and out of scope for this count). PRIMITIVE-GAP: 12/14 (every
other row — uniformly on the events/channels axis, several also introducing
a distinct trigger-shape variant: schedule, on_message, webhook, subscribe,
device-topic-subscribe). DESIGN-CHALLENGE: **0/14 outright**, but one
adjacent honest flag not surfaced by any of the 14 rows directly: a
sub-second-reactive agent (e.g., a trading bot, present in the wider OpenClaw
gallery's Finance category but excluded from this batch as lower-priority
than the top 10) would strain the phase-level checkpoint granularity
(SPEC-AGENTFW-02 §3, explicit non-goal "no true mid-phase checkpointing")
and the `events.subscribe` poll-interval model — flagged for awareness, not
scored, since it wasn't one of the 14 researched rows.

---

[oc-gh]: https://github.com/openclaw/openclaw
[oc-site]: https://openclaw.ai/
[oc-gallery]: https://github.com/mergisi/awesome-openclaw-agents
[herm-cov]: https://petronellatech.com/blog/hermes-agent-ai-guide/
[herm-org]: https://hermes-agent.org/
[herm-gh]: https://github.com/NousResearch/hermes-agent
[herm-hub]: https://agentskillshub.dev/skills/hermes-agent/
[herm-agency]: https://hermesagent.agency/
