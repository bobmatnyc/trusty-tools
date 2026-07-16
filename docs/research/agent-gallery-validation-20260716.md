# Agent Gallery Validation — OpenClaw / Hermes vs. trusty-agents Instructions-Only Design

**Date:** 2026-07-16
**Author:** Research pass, trusty-agents track
**Refs:** #2791 (SPEC-AGENTFW epic), PR #2792 (spec branch `spec-eve-style-agents`),
#2810 (epic: events primitive), #2811 (epic: channels formalization)
**Spec version history (this document was re-baselined mid-flight — read this
before trusting any classification below):**

- **Initial validation pass (below, mostly unchanged):** ran against
  `docs/specs/trusty-agents-eve-style-agents-spec.md` on
  `origin/spec-eve-style-agents` @ `526ff201` — the **v2** rewrite (normative,
  per-section `SPEC-AGENTFW-01..06`), not the v1 draft PR #2792 originally
  merged and rejected by Bob ("the eve research is NOT a spec"). v2 had no
  `events:`/`channels:` primitives at all — §9 made multi-channel adapters an
  explicit non-goal, and the only `Event` concept was the internal
  run-lifecycle SSE stream.
- **Re-baselined (this revision) against v3 @ `f92ef71d`**, which landed on
  the same branch *while this research was in flight*. v3 adds exactly the
  two primitives 12 of this document's 14 rows were blocked on:
  `events: { subscribe, schedule }` (§2.3.6, backed by a **NEW**
  `EventTriggerDispatcher` + `AgentScheduler`) and `channels: { tools,
  inbound }` (§2.3.7, grounded in the real `trusty-channels` crate + the
  existing `telegram`/`slack` inbound gateways — channels is **no longer a
  non-goal**). v3 remains declarative-only end to end (§9: "No coded agents,
  ever" is now a mechanically-enforced load-time rejection, not a soft
  preference) — directly consistent with, and validated by, this document's
  0/14-required-code finding below. Every classification in Phase 2 has been
  re-checked against v3's exact wording; where v2's gap is now spec'd, the
  row is marked **SPEC'D-IN-V3 (implementation pending — epics #2810 events,
  #2811 channels)** rather than removed, so the "what did v2 lack vs. what
  does v3 still lack" history stays visible. A small number of gaps survive
  v3 unchanged — see the inventory and Verdict.

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

Every manifest sketch below is preserved **exactly as originally written
against v2** — `events:`/`channels:` keys and shapes (`cron:`, `webhook:`,
`subscribe: {connector, poll_interval}`, a flat `channels:` list) reflect what
this pass proposed *before* v3 existed, not v3's actual schema (which uses
`events.schedule: "<N>m|h|d"` interval strings, `events.subscribe: [EventEnumVariant, ...]`
against the *internal* bus, and `channels.tools`/`channels.inbound` lists —
see §2.3 of the v3 spec). Each *Classification* line below has been
re-evaluated against v3's real wording rather than silently rewriting the
YAML to match in hindsight, so the gap between "what this research pass
guessed the primitive would look like" and "what the spec author actually
shipped" stays visible.

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

*Classification (v2):* **PRIMITIVE-GAP.** The reasoning/persona/tool-binding
core is fully expressible today (`tools.allowed` against `mcp_task_*` — these
are literally the existing `trusty-memory` MCP tools already in this
environment's tool list — plus `memory.segment: brief`, both per
SPEC-AGENTFW-01/06). What's missing is the wake mechanism: a cron-schedule
event trigger and a channel-delivery binding, neither of which the v2 spec
defines (§9 explicitly makes multi-channel a non-goal/open question; no
schedule/trigger concept exists anywhere in `events.rs`, which is
run-lifecycle-only).

*Re-baselined against v3:* **SPEC'D-IN-V3 (implementation pending — epics
#2810 events, #2811 channels).** `events.schedule` (§2.3.6) covers the wake,
`channels.tools: [telegram, slack]` (§2.3.7) covers delivery. **Caveat:**
v3's `schedule` is a plain interval string (`"1h"`/`"1d"`), not a
wall-clock/day-of-week cron grammar (§9: "No cron-expression scheduling in
the MVP," owner-decision item 8) — "every weekday at 8am" isn't literally
expressible as written; the closest fit is a `"1d"` tick plus agent-side
instructions that no-op outside the target hour/weekday window. This is a
real-world data point reinforcing owner-decision item 8, not a new gap.

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

*Classification (v2):* **PRIMITIVE-GAP** (webhook trigger) **+ SUPPORTED**
(delegation). The escalation handoff is a clean fit for
SPEC-AGENTFW-04's `HandoffContext`/`subagents.allowed` — this is exactly the
declared-subagent-set + structured-handoff pattern the spec already
normatively defines. The blocking gap is purely the inbound-webhook trigger
and a concrete ticketing-system MCP connector (both new).

*Re-baselined against v3:* **Delegation stays SUPPORTED. The webhook trigger
stays PRIMITIVE-GAP — v3 does not close this one.** Checked precisely: v3's
`events.subscribe` only fires on **internal** `Event` enum variants
(`PhaseDone`, `ToolResult`, etc., §2.3.6 — "each entry MUST name a real Event
enum variant"), not arbitrary third-party payloads; `channels.inbound`
(§2.3.7) only wakes an agent on inbound *chat-platform* messages
(Telegram/Slack), not a generic `POST /hooks/<anything>` from a ticketing
SaaS. Grepping the v3 spec text for "webhook" returns zero hits. A true
inbound-HTTP-from-arbitrary-third-party-system trigger is a genuine gap v3
does not cover — the closest workaround is `events.schedule` polling the
ticketing API instead of a real push webhook, trading immediacy for
coverage. **This is the clearest surviving gap in the whole matrix and is
flagged in the Verdict for routing back to the spec author.**

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

*Classification (v2):* **PRIMITIVE-GAP.** Same two gaps as Orion (schedule +
channel), plus a second trigger shape — `on_message` (reactive to inbound
channel text, not just a timer or webhook) — that is a third distinct event
kind the platform needs, not a variant of the other two.

*Re-baselined against v3:* **SPEC'D-IN-V3 (implementation pending — epics
#2810 events, #2811 channels), in full.** `events.schedule` covers the
morning calendar check; `channels.inbound: [slack]` (§2.3.7) is exactly the
"ad-hoc request posted directly in the channel" wake case — v3's own
manifest example even carries `inbound: [slack]` verbatim. Same cron-vs-tick
caveat as Orion applies to the "every morning" phrasing.

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

*Classification (v2):* **PRIMITIVE-GAP.** Introduces the fourth trigger
shape: polling subscription against an external system with no native
webhook (most CRMs' free/starter tiers lack outbound webhooks) — distinct
from the webhook-driven Compass case.

*Re-baselined against v3:* **SPEC'D-IN-V3 (implementation pending — epics
#2810 events, #2811 channels), with one naming correction.** This row's
original manifest used `events.subscribe` to mean "poll an external
connector on an interval" — that is **not** what v3's `events.subscribe`
does (§2.3.6: internal `Event`-enum-variant names only). The v3 primitive
that actually covers "check the CRM for new leads every 15 minutes" is
`events.schedule: "15m"` — the tick fires the agent, and the agent's own
`mcp_crm_read` tool call (inside its instructions) does the "what's new
since last time" check. Functionally equivalent to what this row needed;
the earlier inventory item "`events.subscribe` (polling-subscription
trigger)" should be read as **superseded by `events.schedule`**, not as an
open gap — see the corrected inventory below.

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

*Classification (v2):* **PRIMITIVE-GAP, mechanism-wise EXTEND not NEW for the
connector.** The Gmail MCP tool itself already exists as a precedent in this
very environment (`mcp__claude_ai_Gmail__*`) — so the *credential-brokered
MCP tool* mechanism (SPEC-AGENTFW-03 §4.2 item 3, `credential_ref`) is the
right, already-specified fit; the gap is exclusively the polling-subscription
event trigger and the Telegram channel, not the Gmail connector concept
itself.

*Re-baselined against v3:* **SPEC'D-IN-V3 (implementation pending — epics
#2810 events, #2811 channels).** Same naming correction as Pipeline: the
5-minute poll is `events.schedule: "5m"`, not v3's `events.subscribe`
(internal-bus-only). `channels.tools: [telegram]` covers delivery. Connector
mechanism remains EXTEND (credential-brokered MCP, already precedented by
the live Gmail MCP tool in this environment) — unchanged by v3.

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

*Classification (v2):* **PRIMITIVE-GAP, connector mechanism already
precedented.** The Google Calendar MCP tool exists in this very environment
(`mcp__claude_ai_Google_Calendar__*`), so the calendar-read/write tool
binding is a clean fit for existing `tools.allowed` + `credential_ref`
brokering; only the twice-daily schedule trigger and Telegram delivery are
missing.

*Re-baselined against v3:* **SPEC'D-IN-V3 (implementation pending — epics
#2810 events, #2811 channels).** `events.schedule` + `channels.tools:
[telegram]` cover the wake/delivery pair. Same cron-ceiling caveat as Orion:
"7am and 9pm, specifically" needs either two separate scheduled agents (each
with its own `schedule: "1d"`, offset appropriately, since v3 has no
multi-time-of-day syntax) or one agent on a shorter tick that self-filters —
noted once here, applies identically to every "twice-daily at fixed times"
row in this batch.

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

*Classification (v2):* **PRIMITIVE-GAP** (schedule + multi-channel fan-out —
a useful variant showing `channels:` needs to support delivery to more than
one surface per run, not just a single bound channel).

*Re-baselined against v3:* **Mostly SPEC'D-IN-V3, one connector caveat.**
`events.schedule` covers the daily wake; `channels.tools` is a `Vec<String>`
(§2.3.3 example: `[slack, telegram]`), so multi-channel fan-out is spec'd,
not just single-channel. **Caveat:** v3's concrete grounding names only two
`trusty-channels` binaries — `slack-mcp` and `telegram-mcp`
(`crates/trusty-channels/src/bin/{slack,telegram}-mcp.rs`, §2.3.7). WhatsApp
is not mentioned anywhere in the v3 spec text. The *binding mechanism*
(`channels.tools: [whatsapp]`) is spec'd generically, but whether a
`whatsapp-mcp` binary exists or is planned in `trusty-channels` (epic #2636)
is unconfirmed by this pass — flagged as a connector-completeness question
for epic #2811, not a manifest-schema gap.

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

*Classification (v2):* **PRIMITIVE-GAP**, and the one case in this batch that
motivates a *distinct* trigger primitive rather than a reuse of "subscribe":
MQTT/device-state event streams are push-based and topic-filtered, not
poll-interval based like the CRM/Gmail cases — worth a named
`subscribe.protocol: mqtt` variant in the eventual schema rather than
forcing it through the same shape as REST polling.

*Re-baselined against v3:* **Split verdict — the Telegram half is SPEC'D-IN-V3;
the MQTT half is a genuine surviving gap.** `channels.inbound: [telegram]`
(§2.3.7) covers "responds to Telegram commands" cleanly. The MQTT
sensor-event half does **not** map onto v3's `events.subscribe` — that
primitive is scoped exclusively to the internal `Event` enum (`PhaseDone`,
`ToolResult`, etc., §2.3.6), not to arbitrary external pub/sub protocols like
MQTT topics. There is no `events.webhook`, no device/IoT push primitive, and
no mention of MQTT/IoT anywhere in the v3 text (confirmed by direct search).
**This is the second concrete surviving gap in the matrix**, alongside
Compass's webhook case — both are flagged in the Verdict for the spec
author. A workaround exists (poll Home Assistant's REST API on a short
`events.schedule` tick instead of subscribing to the MQTT push), but it
trades "instant" for "up to one tick late," which matters for a
motion-at-2am alert.

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

*Classification (v2):* **SUPPORTED (core) + PRIMITIVE-GAP (wake/delivery).**
This is the single cleanest existing-primitive fit in the whole batch — the
entire value proposition is `memory.segment: agent_memory` plus the
already-existing `memory_remember`/`memory_recall` MCP tools, exactly as
SPEC-AGENTFW-06 describes. Isolating this one shows the gap in every other
row is structurally the trigger/channel pair, not the reasoning/memory core.

*Re-baselined against v3:* **Fully SPEC'D-IN-V3 (implementation pending —
epics #2810 events, #2811 channels).** `events.schedule` (daily follow-up
surfacing) + `channels.inbound: [telegram]` (ad-hoc "who do I owe a reply
to?") close both remaining gaps cleanly — this row now has zero
un-spec'd primitives, only implementation work.

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

*Classification (v2):* **PRIMITIVE-GAP** (inbound-message trigger + channel;
calendar tool binding itself is precedented/EXTEND, same as Atlas).

*Re-baselined against v3:* **SPEC'D-IN-V3 (implementation pending — epics
#2810 events, #2811 channels).** `channels.inbound: [slack]` is exactly this
row's trigger shape — v3's own worked example even lists `inbound: [slack]`
literally. No caveat here (unlike the schedule rows): a pure
reactive-to-message agent has no wall-clock/cron ceiling to worry about.

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

*Classification (v2):* **PRIMITIVE-GAP.** Directly validates that the
schedule primitive is not a nice-to-have for one or two edge-case agents —
it is Hermes's headline capability, marketed on its own homepage, and it
maps to zero lines of the current trusty-agents spec.

*Re-baselined against v3:* **SPEC'D-IN-V3 (implementation pending — epic
#2810 events).** `events.schedule: "1d"` at 2am-equivalent tick spacing is
the direct fit — this row is the strongest possible validation that
`events.schedule` was the right primitive to add first (§8 Phase 4 already
sequences it as the single largest new-infrastructure investment in v3).

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

*Classification (v2):* **SUPPORTED, with one adapted mechanism.** This is
the best test of whether "no user code, ever" can still capture Hermes's
signature "agent writes its own skill after solving a hard problem" trick
without letting an agent author files. It can: instead of the agent writing
a new `SKILL.md` to disk, the *existing* `memory.segment: agent_memory`
binding (SPEC-AGENTFW-06) already lets a phase write a structured
"how I solved this" record via `memory_remember`, retrievable on a future
run via `memory_recall`. Functionally equivalent, and arguably safer — no
new file lands in the agent's own instructions-only package, so the
never-user-code invariant holds. Only the trigger/channel pair is a gap, not
this mechanism.

*Re-baselined against v3:* **Memory mechanism stays SUPPORTED (v3 doesn't
touch §7/memory). Trigger/channel pair is now SPEC'D-IN-V3 (implementation
pending — epics #2810 events, #2811 channels).** `channels.inbound:
[telegram]` covers "on request" exactly. v3's own §9 non-goal — "No coded
agents, ever," now mechanically enforced at load time, not a lint — is
direct, independent confirmation of this row's original finding: Hermes's
self-generated-skill trick is fully expressible without an agent ever
writing a file into its own package.

### 13. Hermes — `openclaw-migration` skill

```yaml
name: openclaw-migration
model: anthropic/claude-sonnet-4.5
description: "Interactively migrate an OpenClaw SOUL.md persona into a trusty-agents package."

events:
  on_message:                                 # not actually a gap — see Classification
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

*Classification (v2 and v3, unchanged):* **SUPPORTED.** No new primitive at
all — this is exactly a `skills.md`-style procedure (existing #482
directory-package convention) combined with the `memory_recall`/existing-tool
set, invoked directly on the CLI (`tagent run openclaw-migration`), which
never needed an `events:`/`channels:` primitive in the first place — the
original v2 "PRIMITIVE-GAP" annotation on the `on_message: channel: cli` line
was a labeling inconsistency in the initial pass, corrected above; CLI
invocation is just running the binary, not a wake/delivery binding. Included
specifically because it is the one gallery row that requires *zero* new
primitives under either spec version, proving the design isn't
gap-everywhere by construction.

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

*Classification (v2):* **PRIMITIVE-GAP (trigger/channel) + SUPPORTED
(delegation).** Reinforces #2's finding: multi-agent composition via
`extends`/`subagents.allowed`/`HandoffContext` is the strongest-covered part
of the spec; channel/trigger binding is the weakest.

*Re-baselined against v3:* **Delegation stays SUPPORTED. Slack half of
trigger/channel is SPEC'D-IN-V3 (implementation pending — epics #2810
events, #2811 channels); Discord half has the same connector-completeness
caveat as row 7's WhatsApp.** `channels.inbound: [slack]` covers the Slack
wake cleanly. v3 names only `slack-mcp`/`telegram-mcp` as concrete
`trusty-channels` binaries — Discord is not mentioned in the v3 spec text at
all, so `channels.tools: [discord]`/`channels.inbound: [discord]` bind at
the schema level but have no confirmed connector behind them yet. Flagged
for epic #2811, same as WhatsApp.

---

## Aggregated Primitive Inventory (TA-M4/TA-M5 backlog input)

Deduplicated across all 14 rows above. **Status column is re-baselined
against v3 @ `f92ef71d`**; the v2-era status (what this pass originally
found before v3 landed) is kept in the Notes column for history.

| # | Primitive | Status (v3) | Notes |
|---|---|---|---|
| 1 | Agent persona/instructions (`system_prompt.content`/`instructions.md`, frontmatter) | **exists** | SPEC-AGENTFW-01 §2.1/§2.4; both loader tiers. Unchanged v2→v3. |
| 2 | `extends` (single-parent agent inheritance) | **extend** (spec'd §2.5, not yet implemented) | Unchanged v2→v3 — full merge-rule algorithm already normative. |
| 3 | `tools.allowed`/`tools.deny` (MCP tool binding via `ToolExecutor`) | **exists** (allow) / **extend** (deny) | Unchanged v2→v3. |
| 4 | `subagents.allowed` + `HandoffContext` (declared-subagent delegation) | **extend** (spec'd §2.3.2/§5, not yet implemented) | Unchanged v2→v3 — the single best-covered composition primitive in this whole exercise (rows 2, 14). |
| 5 | `memory.segment`/`top_k` (declarative memory-scope binding) | **extend** (spec'd §2.3.3/§7, not yet implemented) | Unchanged v2→v3 — 5 existing Palace segments, just not yet declarable from the agent's own file. |
| 6 | Model/provider resolution (`resolve_model`/`adapter_for_model`) | **exists** | Unchanged v2→v3; v3 additionally clarifies this already routes through the shipped `trusty_common::inference::InferenceAdapter` layer, not a planned one (§2.3.4). |
| 7 | Credential-brokered MCP tool (`credential_ref`) | **extend** (spec'd §4.2 item 3; v3 narrows the open question to one fallback-tension detail, owner-decision item 1) | v3 corrects v2's framing: the keyring/file-store resolver (`trusty_common::inference::credentials::resolver`) is **already shipped**, not a hypothetical backend — the only open question is whether `credential_ref` should inherit its existing plaintext-file fallback or hard-fail. |
| 8 | HTTP-transport MCP client | **extend** (spec'd, not yet implemented) | Unchanged v2→v3. |
| 9 | Checkpoint/resume (phase-level durability) | **extend** (spec'd §2.3.5/§3, not yet implemented) | v3 additionally **permanently decides** phase-level (not sub-turn) granularity is the final design, not an MVP placeholder (§9, Bob 2026-07-16) — relevant to reliability of long scheduled/triggered runs but not one of this gallery's blocking gaps. |
| 10 | Internal lifecycle events (`PhaseStarted`/`PhaseDone`/`RunResumed`, SSE) | **exists** | `events.rs`/`events_sse.rs` — still **not** the same thing as an external trigger; this conflation risk is exactly why v3's own `events.subscribe` is scoped to internal variants only (see #12 below), which this re-baseline had to correct in three rows (Pipeline, Inbox, and the general naming note). |
| 11 | **`events.schedule` (interval-based wake)** | **new → now SPEC'D** (§2.3.6, `AgentScheduler`, epic #2810) | Was the single highest-priority gap in the v2 pass (needed by 8+/14 rows); v3 closes it with a minimal `"<N>m\|h\|d"` interval string. **Surviving caveat:** not cron-expression syntax — no wall-clock/day-of-week targeting (§9 non-goal, owner-decision item 8). Multiple rows in this batch (Orion, Atlas, Morning Briefing, Personal CRM) wanted "at a specific time," which isn't literally expressible yet — real-world evidence to attach to item 8. |
| 12 | **`events.subscribe` (internal Event-bus trigger)** | **new → now SPEC'D** (§2.3.6, `EventTriggerDispatcher`, epic #2810) | **Naming correction from the original inventory:** this pass's v2-era item 14 ("polling-subscription trigger" for external systems like a CRM or Gmail) does **not** map onto v3's `events.subscribe` — v3 scopes `subscribe` strictly to internal `Event` enum variants (`PhaseDone`, `ToolResult`, etc.). The external-polling need those rows actually had is served by `events.schedule` (#11) instead, with the agent's own tool call doing the "what's new" check per tick. `events.subscribe` itself is genuinely useful (e.g., an agent that reacts when *another* agent's phase completes) but wasn't directly needed by any of the 14 researched rows. |
| 13 | **`channels.tools` (chat-platform tool binding)** | **new → now SPEC'D** (§2.3.7, zero new platform code — a documentation/config convention over the existing `tools:` primitive + the `trusty-channels` crate, epic #2636/ADR-0014; per-agent formalization tracked as epic #2811) | Closes the "deliver to a channel" half of the old `channels:` gap for every row that used it (13/14). |
| 14 | **`channels.inbound` (agent-specific inbound-message routing)** | **new → now SPEC'D** (§2.3.7, extends the existing `telegram`/`slack` gateway handlers with a new per-agent routing step, epic #2811) | Closes the old "`events.on_message`" item from the original inventory — v3 folds inbound-message wake into `channels.inbound` rather than a separate `events.on_message` key, which is the correct home for it (it's about *which agent* a channel's messages route to, not a new event kind). Needed by 6/14 rows. **Open per owner-decision item 7:** the exact routing mechanism (per-agent bot/token vs. slash-command prefix vs. chat-id→agent map) isn't chosen yet — implementation-time decision, not a spec gap. |
| 15 | **Generic inbound-HTTP webhook trigger** (arbitrary third-party SaaS push — ticket-created, payment received, CI/CD deploy status) | **still NEW — survives v3 unchanged** | **Confirmed surviving gap.** Zero mentions of "webhook" anywhere in the v3 spec text (direct search). `channels.inbound` is chat-platform-specific; `events.subscribe` is internal-bus-only. Needed by Compass directly and structurally by most Business/DevOps/Finance gallery categories not in this batch (Ledger, Deploy Guardian, Incident Responder, NPS Followup). **Flagged for the spec author — see Verdict.** |
| 16 | **Device/IoT push-subscription trigger** (MQTT-style topic-filtered push, e.g. Home Assistant sensor events) | **still NEW — survives v3 unchanged** | **Confirmed surviving gap.** Zero mentions of MQTT/IoT/device anywhere in the v3 spec text. Distinct from #15 (arbitrary REST webhook) because the protocol shape (persistent topic subscription, not a one-shot HTTP POST) differs. Needed by Home Automation's sensor-reaction half. **Flagged for the spec author — see Verdict.** |
| 17 | **Named SaaS/MCP connectors**: Gmail, Google Calendar, generic CRM, Google Search Console, Home-Assistant, ticketing (Zendesk/Intercom-style), Stripe/payments | **extend** | Unchanged v2→v3 — mechanism (#7) already covers all of these; what's missing is the concrete `McpService` config entries + short skill docs per service, not new platform code. Gmail and Google Calendar already have a working precedent *in this very session's tool list*. |
| 18 | **Channel connector completeness beyond Telegram/Slack** (WhatsApp, Discord, and the rest of `trusty-channels`' eventual roster) | **extend, partially unconfirmed** | v3 names only `slack-mcp`/`telegram-mcp` as concrete existing `trusty-channels` binaries (§2.3.7). Rows 7 (WhatsApp) and 14 (Discord) bind at the `channels:` schema level correctly, but this pass found no v3 text confirming those specific connectors exist or are scheduled — a completeness question for epic #2811, not a schema gap. |
| 19 | **Memory-as-skill-promotion convenience** (`memory.auto_inject: true`) | **new (minor, optional)** | Unchanged v2→v3 — not required (row 12 shows the existing `memory_remember`/`memory_recall` pair already suffices), a nice-to-have only. |

**Final per-row classification, one dominant bucket per row** (several rows
have a SUPPORTED sub-part — memory, delegation — stacked with a remaining
gap; the dominant bucket below reflects whether the row can actually *ship*
end to end, per the detailed per-row Classification entries above):

| Row | Dominant classification |
|---|---|
| 1 Orion | SPEC'D-IN-V3 |
| 2 Compass | **PRIMITIVE-GAP** (delegation half is SUPPORTED; webhook half survives v3) |
| 3 Echo | SPEC'D-IN-V3 |
| 4 Pipeline | SPEC'D-IN-V3 |
| 5 Inbox | SPEC'D-IN-V3 |
| 6 Atlas | SPEC'D-IN-V3 |
| 7 Morning Briefing | SPEC'D-IN-V3 |
| 8 Home Automation | **PRIMITIVE-GAP** (Telegram half is SPEC'D-IN-V3; MQTT half survives v3) |
| 9 Personal CRM | SPEC'D-IN-V3 (memory core was already SUPPORTED even under v2) |
| 10 Meeting Scheduler | SPEC'D-IN-V3 |
| 11 Hermes — scheduled automations | SPEC'D-IN-V3 |
| 12 Hermes — competitor-analysis | SPEC'D-IN-V3 (memory/skill mechanism is SUPPORTED) |
| 13 Hermes — `openclaw-migration` | **SUPPORTED** (no `events:`/`channels:` need at all, unaffected by any spec version) |
| 14 Hermes — team-assistant | SPEC'D-IN-V3 (delegation half is SUPPORTED; Discord is a connector-completeness question, not a schema gap, same status as row 7's WhatsApp) |

**Totals: SUPPORTED 1/14 · SPEC'D-IN-V3 (implementation pending) 11/14 ·
PRIMITIVE-GAP (survives v3) 2/14 · DESIGN-CHALLENGE 0/14** (one adjacent
flag — a sub-second-reactive trading-bot-style agent — noted in the Verdict
but not one of the 14 scored rows).

**Primitive-status counts:** 4 **exist** unmodified (#1, #3-allow, #6, #10);
9 are **extend** (#2, #3-deny, #4, #5, #7, #8, #9, #17, #18); **6 were new
in v2 and are now split 4 SPEC'D / 2 still-NEW** — `events.schedule` (#11),
`events.subscribe`-internal (#12), `channels.tools` (#13), and
`channels.inbound` (#14) are now spec'd (epics #2810/#2811, implementation
pending); the generic inbound-webhook trigger (#15) and the device/IoT push
trigger (#16) are the **two genuine gaps that survive v3 and should be
routed to the spec author**, plus the minor optional #19.

---

## Verdict

*(This section reflects the v3 re-baseline. The v2-era verdict — 13/14 rows
blocked purely on undefined events/channels primitives — is preserved in
spirit above at each row's "Classification (v2)" line; this is the current,
final answer.)*

**Does instructions-only hold?** Yes, on both counts this exercise was
designed to test. First, the **no-user-code invariant**: across all 14
agents, nothing required an agent package to contain executable code, a
runtime-authored file, or a bespoke Rust/Python shim — every row was
expressible as frontmatter + YAML manifest + Markdown instructions,
including the two rows (12, 13) that most directly tested the boundary:
Hermes's self-writing-its-own-skills pattern maps cleanly onto the
`memory.segment`/`memory_remember`/`memory_recall` primitives rather than
requiring the agent to author a new file, and the OpenClaw-migration
meta-agent (row 13) is itself just a `skills.md` procedure. v3 independently
confirms this finding at the spec level: §9 promotes "No coded agents, ever"
from a soft preference to a **mechanically-enforced load-time rejection**,
and this validation pass's 0/14-required-code result is direct, empirical
evidence that the enforcement doesn't cost real-world coverage. Second, the
**composition** dimension: `extends` + `subagents.allowed` + `HandoffContext`
(unchanged v2→v3) cover every multi-agent handoff pattern the gallery
produced (Compass→escalation, team-assistant→scheduler/CRM) without any gap,
in either spec version.

**What changed since the v2 pass: the trigger/delivery gap that blocked
13/14 rows is now closed at the spec level.** v3 adds exactly the two
primitives this document's v2-era inventory asked for —
`events: {subscribe, schedule}` (§2.3.6, `EventTriggerDispatcher` +
`AgentScheduler`) and `channels: {tools, inbound}` (§2.3.7, grounded in the
real `trusty-channels` crate plus the existing Telegram/Slack inbound
gateways) — and channels is explicitly no longer a non-goal (§9, "DECIDED
(Bob, 2026-07-16): channels are explicitly in scope as a primitive"). 11 of
14 rows move from PRIMITIVE-GAP to **SPEC'D-IN-V3**: the manifest schema now
has a place for every wake/delivery need those rows had. **What remains is
implementation, not design** — epic #2810 (events: the `EventTriggerDispatcher`
and `AgentScheduler` are, per v3's own §8 Phase 4, "the largest genuinely new
platform-infrastructure investment in this spec") and epic #2811 (channels:
extending the existing `telegram`/`slack` gateway handlers with per-agent
routing, per owner-decision item 7's three unresolved routing-mechanism
options).

**Two gaps genuinely survive v3, and this is the answer to "what should the
spec still add":**

1. **No generic inbound-HTTP webhook trigger for arbitrary third-party SaaS
   push events** (a ticket-creation webhook, a payment webhook, a CI/CD
   deploy-status webhook — not a chat message). v3's two trigger primitives
   don't cover this: `channels.inbound` is chat-platform-specific by
   definition (§2.3.7), and `events.subscribe` is scoped strictly to
   **internal** `Event` enum variants (§2.3.6 — "each entry MUST name a real
   Event enum variant"), not arbitrary external payloads. A direct text
   search of the v3 spec for "webhook" returns zero hits. This blocked
   Compass (row 2) directly and, by extension, blocks most of the gallery's
   Business/DevOps/Finance categories that weren't in this 14-row batch
   (ticket systems, payment processors, CI/CD platforms, form-completion
   webhooks). **Recommendation to route to the spec author:** an
   `events.webhook: { path: "/hooks/<name>" }` primitive, structurally
   parallel to `events.schedule`, is the natural addition — same
   `AgentRunner::run_with_context` dispatch target as the other two
   triggers, just fed by an inbound HTTP listener instead of a timer or the
   internal bus.
2. **No device/IoT push-subscription trigger** (MQTT-style topic-filtered
   push, e.g., a Home Assistant sensor event). Also zero mentions in the v3
   spec text. This blocked Home Automation's (row 8) sensor-reaction half —
   the Telegram-command half is fully covered by `channels.inbound`, but
   "notify me if motion is detected at 2am" has no real-time trigger to bind
   to; the only available workaround is polling device state on a schedule
   tick, trading immediacy for coverage. **Recommendation:** either fold
   this into a generalized `events.subscribe` (renaming/widening it beyond
   "internal bus only" to also accept an external pub/sub connector
   reference) or a distinct `events.device_subscribe`/`events.mqtt`
   primitive — the spec author should decide which, since it interacts with
   whether `events.subscribe`'s internal-only scoping was a deliberate
   narrowing or an oversight.

Two secondary, lower-priority notes for whoever's tracking epic #2811: (a)
`channels.tools`/`channels.inbound` are schema-general (`Vec<String>`), but
v3's text only names `slack-mcp`/`telegram-mcp` as concrete existing
`trusty-channels` binaries — WhatsApp (row 7) and Discord (row 14) bind
correctly at the manifest level but their actual connector implementations
are unconfirmed by this pass, a completeness question rather than a schema
gap. (b) The `events.schedule` minimal-interval syntax (`"15m"`/`"1h"`/`"1d"`)
has no wall-clock/day-of-week targeting — several rows (Orion, Atlas, Morning
Briefing, Personal CRM) wanted "every weekday at 8am" specifically, which
isn't literally expressible; this is real-world evidence supporting v3's
already-tracked owner-decision item 8 ("confirm this is acceptable long-term,
or flag that cron-expression support... should be pulled forward"), not a new
finding.

**The one adjacent, not-directly-scored flag:** a sub-second-reactive agent
(e.g., a trading bot, present in the wider OpenClaw gallery's Finance
category but excluded from this 14-row batch as lower-priority) would strain
both the phase-level checkpoint granularity (§9: "DECIDED — phase-level
granularity... approved as the permanent design, not a placeholder," closing
what was owner-decision item 1 in v2) and the tick-interval shape of
`events.schedule`. This caveat stands unchanged from the original pass — it
wasn't one of the 14 researched rows, so it isn't counted in the totals
below, but it's worth keeping in view if/when a market-data or
real-time-control use case gets prioritized.

**Final classification totals (14 rows):** **SUPPORTED: 1/14** ·
**SPEC'D-IN-V3 (implementation pending): 11/14** · **PRIMITIVE-GAP (survives
v3): 2/14** (Compass's webhook half, Home Automation's MQTT half — both
partial, not whole-row, gaps) · **DESIGN-CHALLENGE: 0/14** (one adjacent flag
noted above, not scored). Full per-row roll-up in the inventory section
above.

---

[oc-gh]: https://github.com/openclaw/openclaw
[oc-site]: https://openclaw.ai/
[oc-gallery]: https://github.com/mergisi/awesome-openclaw-agents
[herm-cov]: https://petronellatech.com/blog/hermes-agent-ai-guide/
[herm-org]: https://hermes-agent.org/
[herm-gh]: https://github.com/NousResearch/hermes-agent
[herm-hub]: https://agentskillshub.dev/skills/hermes-agent/
[herm-agency]: https://hermesagent.agency/
