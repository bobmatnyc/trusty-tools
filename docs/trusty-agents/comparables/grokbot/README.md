# Grok Bot — Competitive/Comparable Research

Research pass on **Grok Bot** (SpaceXAI + Cursor; often styled GrokBot in conversation), captured as a comparable for trusty-agents design work.

**Research date:** 2026-09-05 (America/New_York)  
**Scope:** Public product positioning, official docs, launch/forum announcements, and security documentation only. No private runtime internals, system prompts, or unpublished architecture.

Contents of this folder:

```
comparables/grokbot/
  README.md                 <- this file (main research writeup)
  raw/                       <- verbatim / cleaned doc-page extracts (markdown), one file per URL
  screenshots/
    marketing/               <- product landing + news page captures + hero asset
    docs/                    <- official docs page captures (x.ai + cursor.com)
```

Screenshots were captured live from public pages on 2026-09-05 via browser automation on the research machine.

---

## 1. What Grok Bot Is

Grok Bot is a **hosted multi-agent product** that reframes AI assistants as **named digital teammates ("Bots")**. Each Bot is a durable agent with a name, job/role description, conversation, and compounding context. Bots run on a **persistent cloud computer** (browser, filesystem, terminal) so work finishes **inside the user's real apps and websites**, not as chat drafts the human must copy elsewhere.

Public one-liner (product page): *"AI teammates you can give real work to. Bots can sign in to your tools, use them just like you do, and come back with finished work."*

Key category claim vs. classic chat assistants:

| Classic assistant | Grok Bot (as positioned) |
|---|---|
| Answers in chat | Completes work in apps/tools |
| Session resets / ephemeral | Named Bot with durable memory + shared computer state |
| Human stays in the loop for every step | Parallel Bots; human pulled in for approvals / judgment |
| Workflow builders / brittle automations | Message-first setup; teach-by-demo → skill → routine |

Vendors: **SpaceXAI** (product brand / x.ai) and **Cursor** (account, hosting, docs, desktop/mobile apps). Auth is Cursor-account-based; SuperGrok subscriptions can also unlock access.

---

## 2. Positioning & Timeline

| Date | Event |
|---|---|
| 2026-08-11 | Early beta launch. Desktop + iOS. Initial access: SuperGrok Heavy, Cursor Ultra, Cursor Teams Premium. Enterprise waitlist. |
| 2026-08-26 | Access expansion: all SuperGrok tiers, Cursor Pro / Pro+ / Ultra, Cursor Teams Standard + Premium. Separate Grok Bot usage pool (does not burn Cursor/Grok plan quotas). |
| 2026-09 (ongoing) | Docs mature across docs.x.ai and cursor.com; enterprise security pages published; community reports on routines queue lag. |

**Positioning pillars** (from launch + product page):

1. **Computer of its own** — persistent cloud VM; connectors/MCP when available; computer use otherwise.
2. **Bots coordinate** — shared computer, per-Bot screens, messaging / group chats / ownership handoffs.
3. **Show how it's done** — follow-along demonstration becomes a reusable skill/routine.
4. **Text-thread UX** — message like a teammate from desktop or phone; same thread everywhere.
5. **Judgment-call interruptions only** — finish end-to-end; stop for approvals / human-only steps.

---

## 3. Surfaces / Clients

| Surface | Notes (public) |
|---|---|
| Desktop app | macOS (Apple silicon + Intel), Windows (x64 + Arm64). FAQ also lists Linux (.deb / .rpm / AppImage). Some Cursor overview copy says "no Linux desktop" — treat as doc inconsistency; FAQ is more specific. |
| Mobile | iPhone (iOS 18+); Android 9+. iPad not supported at initial launch. |
| Agent Computer preview | In-app view of the shared cloud desktop (clicks, typing, navigation, status). |
| Cloud computer | Always-on hosted Linux environment; work continues when laptop/app closed. |
| Local computer (optional) | Separate capability: Bot can run commands on the user's Mac/Windows under a local-execution policy (ask every time / always / never). |

Conversation is the primary UX. There is **no required workflow builder**. Create a Bot → message a job → grant access as asked.

Account limits (public): up to **50 Bots and group chats combined**.

---

## 4. Architecture (as published)

### Shared computer, per-user isolation

- **All Bots on one account share one cloud computer.**
- Shared: browser cookies/sessions, `/workspace` files, CLI credentials, installed packages (treat ephemeral installs as replaceable).
- **Per Bot:** own screen / conversation / learned memory / profile. Screens enable parallel computer-use (one computer-use task per screen at a time).
- **Screens are work surfaces, not security boundaries.** Docs repeatedly warn: do not use separate Bots as isolation.
- **Between users:** isolation is strict (dedicated, hardware-isolated computers per published security docs).

### Connector-first, GUI fallback

```
Prefer: Plugins / connectors (MCP-backed structured tools)
   ↓ if unavailable or visual-only
Fall back: Browser + desktop computer use
   ↓ if human-gated
Hand computer to user (password / 2FA / CAPTCHA / payment)
```

Connector OAuth tokens stay on **Cursor's connector backend**; Bots invoke tools without receiving tokens on the computer.

### Delegation surfaces (named in security docs)

Auto Review covers (among other things) shell, plugin calls, computer use, automation writes (routines/triggers), and **delegation such as Cloud Agent and subagent launches**. Public docs position Cloud Agents as a coding/delegation path related to Cursor's broader agent stack, distinct from day-to-day Bot chat.

### Hosting & egress

- Cursor-hosted computers only (US today).
- No on-prem, no BYO image, no customer VPN/private link for core product.
- Shared static egress IPs (not per-customer).
- Enterprise **Network Controls** allow destination allowlists; Team Setup can install networking clients (e.g. Tailscale / Cloudflare Tunnel) for private services.

---

## 5. Design Principles (extracted)

These are the **product design principles** implied/stated across public docs — useful as a comparable checklist for trusty-agents.

### P1. Teammate metaphor over chatbot metaphor
UX is messaging a coworker: jobs, ownership, handoffs, "come back when you need me." Roles are first-class (Sales Outbound, Talent Scout, Bug Reproduction, Chief of Staff, …).

### P2. Outcome-defined jobs live in the Bot description
Standing boundaries and role ("never contact customers"; "own weekly account-health") go in the **description**. Task-specific instructions stay in messages. Description = durable policy; chat = instance.

### P3. Finish in the tool of record
Success criterion is an artifact in Salesforce / Gmail / staging / spreadsheet / PR — not a paragraph the user retypes. Prefer connectors; use computer use for the long tail.

### P4. Shared substrate enables handoffs; isolation is per-user
Explicit tradeoff: collaboration convenience (shared logins/files) over per-Bot sandboxing. Security docs make this a first-class teaching point.

### P5. Skills (how) vs routines (when)
- **Skill** = reusable procedure (steps, decision rules, outputs, approval boundaries). Cross-Bot library; may need connector/login to run.
- **Routine** = assigns a workflow to **one Bot** + schedule or event trigger.
- Pipeline: one-time task → reliable → save skill → test → only then automate.
- **Teach a task**: record ≤10 minutes of visible computer interaction → draft skill (no mic audio). Gradual rollout.

### P6. Draft/prepare before execute
Routines and use-case starters emphasize research, reconcile, recommend, leave review lists. Sending / publishing / purchasing / deleting / production changes stay behind approval.

### P7. Defense in depth for agency
Layers (public):
1. Boundaries stated in the request / Bot description  
2. Per-action approval cards  
3. **Auto Review** (independent review model on risky tool/computer/automation/delegation actions)  
4. Network policy (Enterprise)  
5. Untrusted marking of outside content (prompt-injection awareness)  
6. Human takeover for secrets (never paste passwords into chat; secure secret-request UI when offered)

### P8. Context compounds, but sources of truth stay external
Bots retain preferences, role context, summaries. For consequential decisions, docs tell users to make the Bot **re-check current systems** rather than trust memory alone.

### P9. Message-first setup, progressive disclosure of power
No YAML/workflow graph required to start. Power features (plugins, skills, routines, groups, Auto Review rules, local execution) appear as the work demands them.

---

## 6. Capabilities Deep Dive

### 6.1 Bots
- Create via sidebar New / Cmd+N → Create new agent; edit profile (name, title, description, avatar).
- Organize: pin, Sidebar Sections (sync desktop↔iOS), hide without deleting (routines keep running), duplicate (copies profile/settings/skills/routines/avatar; **not** history/memory/attachments).
- Share: public template link copies **configuration** only — not computer, logins, or conversation. Teams/Enterprise admins can restrict public template publishing.
- Delete: removes profile, conversation, routines; shared computer files/sessions may remain (explicit cleanup required).

### 6.2 Messaging & collaboration
- Paste text/links/images; attach files; `/` skills; `@` Bots/groups/routines/connectors; threads; reactions; mid-flight redirects; "Stop now" (does not undo completed side effects).
- Attachment limits (desktop): 6 per message; 25 MB docs/images/audio; 200 MB video.
- **Group chats:** 2–6 Bots; assign stage owners; Bot-to-group handoffs are text-only (images go 1:1 when a Bot must inspect).
- Async Bot↔Bot messaging with visible handoffs in the thread.

### 6.3 Computer & apps
- Shared `/workspace` for durable artifacts.
- Watch via Agent Computer; take over for human-only steps; return control.
- Computer lifecycle: retry → app restart → Recover → Update image → Reset (last resort). Recover/Update preserve durable state; Reset returns to synced durable snapshot.
- Local execution is a **separate** policy from cloud-computer approvals.

### 6.4 Plugins / connectors
- Settings → Plugins (or in-chat Connect cards).
- Account-wide once installed.
- Prefer over raw browsing when available.
- Team connector policy can disable plugins; that does **not** block the website — Network Controls is the second door (Enterprise).

### 6.5 Skills & routines
- Skill authoring: ask to save a process; or Teach a task.
- Good skill contents: when to use, inputs/access, sequence, validation, return format, approval gates.
- Routines: cron (user TZ) or event triggers via **Cursor account integrations** (Slack message, GitHub notification, etc.) — **separate from** Slack/GitHub plugins.
- Limits: ≤50 routines per Bot; 20 most recent run records retained; delete is immediate/no undo.
- Test run performs **real** work — use safe inputs.
- After long inactivity, product may ask to keep routines running / pause them.
- Community signal (forum, Sep 2026): scheduled routines can sit in a server queue ~10–37 minutes after fire time; silent success if the routine writes files but doesn't chat. Label as **community report**, not official SLA.

### 6.6 Approvals & Auto Review
- Approval cards show proposed operation + inputs; Allow once / Always allow / Deny (mobile: Approve once / Deny).
- Auto Review: model-based gate on risky actions; personal Require Approval / Always Allow rules; Require Approval wins on conflict; team-wide allow/block instructions (Teams security settings).
- Not a complete side-effect reviewer (memory writes / many settings changes called out as examples of gaps).
- Organization-level lock forcing Auto Review on: **not available** (as of security docs).

---

## 7. Security & Trust Model

| Control | Who | Notes |
|---|---|---|
| Per-action approvals | Member | Strongest practical boundary is wording in the request |
| Auto Review | Member (+ team instructions) | Enforcement currently on for all users; personal setting can disable |
| Local execution policy | Member / team ceiling | Default ask every time; recommend Never unless needed |
| Network Controls | Enterprise | Destination allowlist modes; directory groups; recreate computer to apply |
| Team Setup | Enterprise | Install customer networking / tooling on every team computer |
| Audit Logs | Enterprise | Control-plane events (Bot create, access, MCP auth, Slack links, routines, …) |
| Action Recording + OTel | Enterprise | Scrubbed Bot actions; 90-day internal retention; `cursor.surface=grok_bot` |
| SCIM | Enterprise | Provision/deprovision |
| MCP allowlist | Enterprise | Connector governance |
| SSO | Teams/Enterprise via Cursor | SAML 2.0 (Okta, Entra, Google Workspace, OneLogin) |
| Privacy Mode | Team/account | Required data storage for Grok Bot — **Legacy Privacy Mode unsupported** |
| Certifications | Org | Anysphere ISO/IEC 27001 + 42001; Grok Bot in ISO scope (trust.cursor.com) |

**Identity model:** Bots have **no separate machine identity**. They act as the signed-in member (except team-managed connectors that may use service accounts). Attribution stays with the human.

**Prompt injection:** Outside content (web, plugins, command output) marked untrusted; Auto Review + network + approvals reduce but do not eliminate risk.

---

## 8. Pricing & Access

Public matrix (subject to change; verify on pricing pages):

- Included with paid Cursor individual plans and Cursor Teams (after Aug 26 expansion); also via SuperGrok / Plus / Heavy.
- **Separate weekly Grok Bot usage** — handoffs do not consume normal Cursor/Grok quotas.
- If both Cursor and SuperGrok present, product uses whichever has more usage (FAQ).
- On-demand usage available on eligible accounts (model/token billed).
- Enterprise: rolling access; admin controls gated as above.

Product page list pricing examples (marketing): Cursor Pro from ~$20/mo; SuperGrok from ~$30/mo; Teams from ~$40/seat/mo — treat as marketing snapshots, not contractual.

---

## 9. Use-Case Patterns

Official starter roles emphasize **read → prepare → review**, then optional automation:

| Role | Owns | Typical stop line |
|---|---|---|
| Sales Outbound | Research, ICP scoring, draft outreach | Do not send/enroll |
| Talent Scout | Sourcing + outreach drafts | Do not contact candidates |
| Paid Media | Spend/performance + reallocation recs | Do not change budgets / send Slack |
| Expense Manager | Weekly reconcile + draft follow-ups | Do not send / change reimbursements |
| Product Performance | Latency/incident investigation packs | Do not change prod/alerts |
| Bug Reproduction | Staging repro packs | No production customer data |
| Account Health | Ranked churn/expansion watch lists | Do not contact / edit CRM |
| Chief of Staff | Priority-linked daily digests | Do not send / change meetings |

Durable Bot recipe (docs):

1. Job + systems + output format + standing boundaries in description  
2. One real safe-scoped task  
3. Correct until reviewable  
4. Save skill  
5. Test on second input  
6. Routine only when retries/failures defined  
7. Keep consequential external actions behind approval  

---

## 10. Implications for trusty-agents

Useful comparable lenses (not a feature parity checklist):

1. **Hosted teammate OS vs. open agent harness**  
   Grok Bot sells a complete product (clients + cloud computer + connectors + approvals). trusty-agents / trusty-mpm sit closer to an open, inspectable multi-agent platform operators can run themselves. Different trust and customization stories.

2. **Shared computer as collaboration primitive**  
   Grok Bot bets that shared sessions/files beat per-agent sandboxes for handoffs. trusty-agents designs that isolate agents harder should call out the collaboration tax; designs that share state should copy Grok Bot's loud documentation of the non-boundary.

3. **Skills vs routines as the automation grammar**  
   Clean public taxonomy: *how* (skill) vs *when* (routine) + teach-by-demo. Comparable to skill packs + schedulers/event buses in trusty-mpm — good naming/UX model to steal.

4. **Connector-first with GUI long tail**  
   Same shape as MCP-preferred + browser fallback. Grok Bot's explicit "tokens never land on the computer" is a strong security talking point for connector design.

5. **Approval + independent review model**  
   Auto Review as a second model judging tool calls is a productized pattern for high-agency agents. Worth comparing to trusty-agents guardrails / pm_guard style gates.

6. **Event triggers via account integrations, not only plugins**  
   Separating "Slack plugin tools" from "Slack event that wakes a routine" is an important architecture distinction for any agent platform.

7. **Chat-primary UX**  
   Power users still live in a messenger. IDE/CLI-native platforms compete on developer intimacy; Grok Bot competes on cross-app ops/GTM/support work.

8. **Enterprise gating**  
   Network policy, action recording, SCIM, MCP allowlist are Enterprise-only — self-serve teams get the agency without the governance dials. Relevant when positioning trusty-agents for regulated buyers.

9. **Operational reality of routines**  
   Community queue lag reports are a reminder: scheduled agents need visible run history, idempotency, and non-silent failure modes — areas trusty-mpm already stresses.

---

## 11. Source Index

Primary official:

- https://x.ai/bot — product landing  
- https://x.ai/news/grok-bot-more-plans — access expansion (2026-08-26)  
- https://docs.x.ai/grok-bot — overview hub  
- https://docs.x.ai/grok-bot/computer-and-apps  
- https://docs.x.ai/grok-bot/approvals-security-and-privacy  
- https://docs.x.ai/grok-bot/skills-routines-and-automations  
- https://docs.x.ai/grok-bot/use-cases  
- https://docs.x.ai/grok-bot/faq  
- https://cursor.com/docs/grok-bot — Cursor docs overview  
- https://cursor.com/docs/grok-bot/work — day-to-day work guide  
- https://cursor.com/docs/grok-bot/security — enterprise/security deep dive  

Launch / press:

- https://forum.cursor.com/t/introducing-grok-bot/168053 — staff launch post (2026-08-11)  
- https://9to5mac.com/2026/08/21/grok-bot-is-an-all-new-iphone-and-mac-app-from-spacexai-and-cursor/  
- https://venturebeat.com/orchestration/spacexais-grok-bot-turns-agents-into-persistent-digital-coworkers-that-can-operate-your-apps-for-120-per-month  

Community signal (label carefully):

- https://forum.cursor.com/t/grok-bot-routines-dont-auto-run-on-schedule/170358 — routine queue lag discussion  

Cleaner extracts live in `raw/`. Screenshot evidence lives in `screenshots/`.

---

*End of comparable research writeup. Update this file when public docs or access matrix change materially.*
