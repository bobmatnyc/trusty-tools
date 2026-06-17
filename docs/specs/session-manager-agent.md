# DOC-14 — Session Manager (SM) Agent

**Status:** Draft
**Subsystem:** trusty-mpm — daemon / session-manager agent
**Owner:** Engineering (trusty-mpm)
**Last-updated:** 2026-06-15
**Spec ID:** `SPEC-SM-AGENT-01~draft` (DOC-14)
**Builds on:** DOC-13 — Coordinator TUI (`docs/specs/tui-coordinator.md`, PR #1271)
**Prior art (reuse targets):**
- trusty-review multi-provider LLM abstraction (`crates/trusty-review/src/llm/`)
- trusty-agents direct-Anthropic native path (`crates/trusty-agents/src/llm/anthropic_native/`)
- trusty-mpm PM instruction composition (`crates/trusty-mpm/src/core/instruction_pipeline.rs`, `instruction_overrides.rs`, `src/assets/instructions/`)
- open-mpm PM_INSTRUCTIONS (`~/Projects/open-mpm/.claude-mpm/PM_INSTRUCTIONS.md`)
**Cross-ref:** session-manager control surface (`crates/trusty-mpm/src/daemon/api.rs`,
`daemon/services/session_service.rs`, `bin/tm/commands/session.rs`), coordinator
endpoints (`daemon/coordinator.rs`, `daemon/api/coordinator_routes.rs`,
`daemon/llm_overseer.rs`), the `session-manager-driver` skill, and issues
**#1269** (trust-dialog / headless-spawn blocker), **#1272** (SM TUI epic).

> **Scope note.** This is a **behavior + architecture requirements** spec for the
> **session manager (SM) agent** — the LLM brain the operator talks to in the
> coordinator TUI input box (DOC-13). It specifies *what the SM agent is*, its
> instructions/system prompt, its inference providers, its context engine, its
> memory model, and its goal tracking. It does **not** implement anything. The PR
> that carries this doc opens **no** Rust changes.

---

## 0. Terminology — "session manager (SM)", not "coordinator"

The operator-facing brain is the **session manager (SM)**. The existing code uses
the name **coordinator** in several load-bearing places:

| Surface | Current name | Files |
|---|---|---|
| REST endpoints | `/api/v1/sessions/context`, `/api/v1/sessions/chat` (renamed from `/api/v1/coordinator/*` in #1392) | `daemon/api.rs`, `daemon/api/coordinator_routes.rs` |
| Daemon module | `coordinator` | `daemon/coordinator.rs`, `daemon/mod.rs:16` |
| CLI | `tm coordinator` | `bin/tm/cli.rs:166` |
| GUI bridge | coordinator commands | `trusty-mpm-gui/src/commands.rs:160-210` |
| TUI | coordinator chat pane | `tui/dashboard/mod.rs`, `tui/mod.rs::coordinator_send` |

### 0.1 Rename / alias decision (NORMATIVE)

The spec **standardizes on "session manager (SM)"** in all new prose, types,
config, prompts, and docs. For the existing wire/CLI surface we adopt a
**keep-and-alias** policy to avoid a breaking change mid-flight:

- **D0.1 — Endpoints: keep `/api/v1/coordinator/*` as the stable path; add
  `/api/v1/session-manager/*` aliases** routed to the same handlers. The
  > **Superseded by #1392:** the `/api/v1/coordinator/*` paths were retired and
  > unified under the plural `/api/v1/sessions/*` namespace
  > (`/api/v1/sessions/context`, `/api/v1/sessions/chat`); the
  > `/api/v1/session-manager/*` aliases remain.
  coordinator paths remain valid (DOC-13 TUI and `trusty-mpm-gui` already bind
  them) and are marked *legacy alias* in OpenRPC/route docs. A future major
  version may retire the coordinator paths; that retirement is out of scope here.
- **D0.2 — CLI: keep `tm coordinator`; add `tm sm` (and `tm session-manager`) as
  aliases** via clap `visible_alias`. No behavior change.
- **D0.3 — New Rust types use the `SessionManager*` / `Sm*` prefix** (e.g.
  `SessionManagerAgent`, `SmContextEngine`, `SmConfig`). The internal
  `LlmOverseer` (`daemon/llm_overseer.rs`) is superseded by the SM agent (§7,
  TICKET SM-6) but its module may retain its name until removed.
- **D0.4 — Config section is `[session_manager]`** in `~/.trusty-mpm/config.toml`
  (note: the config file is `config.toml`, parsed into `MpmConfig` at
  `core/config.rs:134`; the brief's `config.yaml` reference is corrected to TOML
  to match the existing loader).

Rationale: "coordinator" is an overloaded term (it also names the TUI in DOC-13
and a dashboard); "session manager" names the *agent's job* — it manages
sessions. Aliasing keeps DOC-13 and the GUI working while the new name becomes
canonical.

---

## 1. Purpose & Goals

### 1.1 What we are building

The **session manager (SM) agent** is the LLM brain that powers the coordinator
TUI input box (DOC-13). The operator types natural language; the SM agent
interprets intent, **delegates every unit of real work by launching a t-mpm
session** (it never edits, researches, or runs ops itself), observes those
sessions, verifies their output, tracks goals across them, and reports back.

It is the PM analogue for the *fleet*: where the trusty-mpm PM (`PM_INSTRUCTIONS`)
delegates work to *agents within one Claude Code session*, the SM delegates work
to *whole Claude Code sessions* and orchestrates many concurrently.

### 1.2 Goals

- **G1 — Delegate-all-via-session.** The SM never does work directly. Every unit
  of work becomes a launched t-mpm session (mirrors the PM prohibitions model).
- **G2 — Multi-provider inference.** Config-selectable OpenRouter / AWS Bedrock /
  direct Anthropic, reusing the trusty-review provider pattern + trusty-agents'
  Anthropic-native path, with a precedence/fallback policy.
- **G3 — Infinite context.** A rolling 10-round verbatim window plus a growing
  compressed-context block, so a conversation never truncates.
- **G4 — Dedicated memory.** The SM owns a dedicated trusty-memory palace
  (distinct from project and personal palaces) and reads/writes it for goals,
  session outcomes, decisions, and cross-session knowledge.
- **G5 — Goal tracking.** The SM tracks goals across sessions: each goal maps to
  one-or-many launched sessions and is surfaced in the TUI and in the SM's
  reasoning.
- **G6 — Graceful degradation without a key.** With no provider credentials, the
  SM degrades to a deterministic routing/triage mode (the DOC-13 G5 contract):
  routed commands and session management still work; free-text reasoning returns
  a "no inference configured" notice.

### 1.3 Non-goals (see §12)

Implementing the SM in Rust; the TUI rendering (DOC-13, the UAT UI built *after*
the stdio API — §1A.2); the deferred Telegram/Slack/Web UIs (§1A.3); the
trust/headless-spawn auto-accept (#1269); replacing the `tm` CLI; per-project
palace creation for the *spawned sessions* (that is separate work — §8.5).

> **Interface-first note:** the **primary** interface for the initial build is
> **direct JSON-RPC over STDIO** (§1A); all SM functionality is validated
> headlessly over stdio before any UI is built.

---

## 1A. Interface strategy & roadmap (API-first) — NORMATIVE

> **Read this before §2.** This section fixes *how* the SM is driven and *in what
> order* the interfaces are built. The headline decision: **build the SM
> API-first**, validate **all** functionality headlessly over **direct JSON-RPC
> over STDIO** before any UI exists, then build the TUI for UAT, and defer the
> richer UIs (Telegram / Slack / Web) to their own future tickets.

### 1A.1 Primary interface = direct JSON-RPC over STDIO (NORMATIVE)

- **D1A.1 — The SM core is interface-agnostic.** All SM behavior (chat,
  orchestration, goals, session delegation, context, health) lives behind a
  transport-neutral core. **The first and primary transport is a STDIO JSON-RPC
  adapter**, so an external driver — e.g. a parent `claude-mpm` / PM agent — can
  drive the SM **headlessly** to exercise **every** capability without any UI.
- **D1A.2 — Wire conventions.** **JSON-RPC 2.0 over stdio**, one request/response
  object per line (newline-delimited), mirroring the existing trusty-* MCP
  `serve --stdio` pattern (`trusty-memory serve --stdio`, `trusty-search serve`).
  Requests carry `{ "jsonrpc": "2.0", "id": <n>, "method": "<m>", "params": {…} }`;
  responses carry `{ "jsonrpc": "2.0", "id": <n>, "result": {…} }` or
  `{ …, "error": { "code", "message", "data" } }`. Logs go to **stderr only**
  (CLAUDE.md daemon convention) so stdout stays clean for JSON-RPC framing.
- **D1A.3 — The adapter is a thin mapping**, not new logic: it maps each method
  onto the existing session-manager control surface (§2.6) + the SM agent loop
  (§3.4). No business logic lives in the transport.

#### First-cut JSON-RPC method surface (NORMATIVE starting point)

| Method | Params (shape) | Result | Maps onto |
|---|---|---|---|
| `sm.chat` | `{ message, conv_id? }` | `{ reply, conv_id, cost? }` | SM agent loop (§3.4) + working-prompt assembly (§7.5) |
| `sm.goals.list` | `{ status? }` | `{ goals: [Goal] }` | Goal model (§9) |
| `sm.goals.create` | `{ description, acceptance? }` | `{ goal: Goal }` | Goal create (§9.2) |
| `sm.goals.update` | `{ id, status?, progress?, note? }` | `{ goal: Goal }` | Goal update (§9.2) |
| `sm.sessions.launch` | `{ workdir, model?, prompt?, goal_id? }` | `{ session_id }` | `POST /sessions` (§2.6) |
| `sm.sessions.list` | `{}` | `{ sessions: [...] }` | `GET /sessions` (§2.6) |
| `sm.sessions.get` | `{ session_id }` | `{ session, output?, events? }` | `GET /sessions/{id}` + `/output` + `/events` (§2.6) |
| `sm.sessions.send` | `{ session_id, text }` | `{ ok }` | `POST /sessions/{id}/command` (§2.6) |
| `sm.sessions.stop` | `{ session_id }` | `{ ok }` | `DELETE /sessions/{id}` (§2.6) |
| `sm.sessions.resume` | `{ session_id }` | `{ ok }` | `POST /sessions/{id}/resume` (§2.6) |
| `sm.sessions.kill` | `{ session_id }` | `{ ok }` | force-stop / reap (`DELETE /sessions/{id}`, `/sessions/dead`, §2.6) |
| `sm.context.get` | `{ conv_id? }` | `{ compressed_context, recent_rounds, total_rounds, token_estimate }` | Context engine state (§7.1/§7.5) |
| `sm.health` | `{}` | `{ ok, provider, degraded, model_tiers }` | Provider/degraded status (§5.3) |

> These map directly onto the existing session-manager API (§2.6) and the SM
> agent loop (§3.4); the adapter resolves `goal_id ↔ session_id` links (§9.3) so
> `sm.sessions.launch` with a `goal_id` records the link.

### 1A.2 Build & test sequence (NORMATIVE ordering)

Validate the SM **API-first**, then build the UAT UI:

1. **STDIO JSON-RPC first.** Stand up the SM core + STDIO JSON-RPC adapter and
   validate **all** SM functionality headlessly over stdio. The reference test
   topology is:

   ```
   claude-mpm (parent / PM)
        ⟷  SM  (JSON-RPC over stdio)
              ⟷  t-mpm (session-manager + spawned Claude Code sessions)
   ```

   The parent `claude-mpm`/PM drives `sm.*` methods over stdio; the SM delegates
   by driving t-mpm's session-manager (§2.6), which spawns/observes the Claude
   Code sessions. **Every** capability (chat, goals, launch/observe/verify,
   context, health) must be exercisable and verifiable over stdio with no UI.
2. **TUI for UAT, second.** Only after stdio validation, build the TUI
   (the coordinator/SM TUI — DOC-13 / PR #1271 / epic **#1272**) for
   user-acceptance testing. **Test the TUI using the `tmux` agent** (the
   `session-manager-driver` / tmux harness), driving the TUI panes the same way
   the driver drives sessions.
3. **Richer UIs, later.** Telegram, Slack, and Web UIs are **deferred** to their
   own future tickets (§1A.3) — each is a thin front-end over the same SM core +
   JSON-RPC/HTTP surface.

### 1A.3 Future UIs (DESIGN FOR, DO NOT BUILD NOW)

All future UIs are **thin front-ends over the SM core** and the **same
JSON-RPC/HTTP method surface** (§1A.1). They are explicitly **out of scope for the
initial API-first build**; each becomes its own future ticket (§14 FUTURE group):

| Future UI | Status | Notes |
|---|---|---|
| **(a) TUI** (coordinator/SM TUI) | DEFERRED — UAT UI, **built after stdio** | DOC-13 / PR #1271 / epic **#1272**; tested via the `tmux` agent. |
| **(b) Telegram bot UI** | DEFERRED — future ticket | Thin front-end over SM core; chat + status over Telegram. |
| **(c) Slack bot UI** | DEFERRED — future ticket | Thin front-end over SM core; chat + status over Slack. |
| **(d) Web (console) UI** | DEFERRED — future ticket | Thin front-end over SM core; lives in `trusty-console` / `trusty-mpm-gui`. |

The SM core must be designed so these attach without core changes: they all speak
the JSON-RPC surface (or the aliased HTTP endpoints, D0.1). Building any of them
now is out of scope.

---

## 2. Background: current state (investigation findings)

### 2.1 Current SM/coordinator inference

Today the coordinator chat is a **thin OpenRouter call**, not an orchestrator:

- Handlers: `coordinator_context` (`GET`, `coordinator_routes.rs:77-81`) and
  `coordinator_chat` (`POST`, `coordinator_routes.rs:107-152`).
- A `@prefix:`/`prefix:` message is routed straight to a tmux pane — no LLM
  (`coordinator_routes.rs:115-125`). A plain message calls
  `overseer.chat(&mut history, &body.message)` (`coordinator_routes.rs:142-145`).
- The real call lives in `daemon/llm_overseer.rs`: POST to
  `https://openrouter.ai/api/v1/chat/completions` (`llm_overseer.rs:24`, `189-202`)
  via `reqwest`, `temperature: 0.7`, bearer `OPENROUTER_API_KEY`. **OpenRouter
  only** — no Bedrock, no direct Anthropic.
- Model config is `[llm]` in `~/.trusty-mpm/framework/hooks/overseer.toml`
  (`core/overseer_config.rs:79-99`): `enabled` (default `false`, opt-in),
  `model` (default `meta-llama/llama-3.1-8b-instruct:free`), `api_key_env`
  (default `OPENROUTER_API_KEY`). Built only when `enabled` (`state/overseer.rs:89-116`).
- **History is client-owned; the daemon is stateless** (`coordinator_routes.rs:26-31`).
  The snapshot prompt is prepended as a synthetic first turn
  (`coordinator_routes.rs:136-140`); history is capped at 20 messages inside
  `LlmOverseer::chat` (`llm_overseer.rs:43`, `392-397`). No persistence, no
  compaction, no memory, no goals, no delegation.

**Gap:** the coordinator chat is a stateless chatbot. The SM agent (this spec)
replaces that brain with an orchestrator that delegates, remembers, compacts, and
tracks goals.

### 2.2 Reuse target — trusty-review multi-provider abstraction

`crates/trusty-review/src/llm/` is a non-streaming, cost/latency-aware,
**prefix-routed** provider factory:

- Trait `LlmProvider::complete` (`llm/mod.rs:138-156`).
- Routing `resolve_provider_and_model(model, default)` (`llm/mod.rs:202-210`):
  `bedrock/…` → Bedrock, `openrouter/…` → OpenRouter, bare → `default_provider`;
  the prefix is stripped before the bare id is sent upstream.
- Factory `build_provider` (`llm/mod.rs:224-240`).
- Providers: `OpenRouterProvider` (`llm/openrouter.rs`, reqwest, bearer
  `OPENROUTER_API_KEY`); `BedrockProvider` (`llm/bedrock/mod.rs`, AWS Converse
  via `aws-sdk-bedrockruntime`, default credential chain, region from
  `TRUSTY_AWS_REGION` > `AWS_REGION` > `us-east-1`).

This routing logic is **internal to trusty-review**, not yet shared.

### 2.3 Reuse target — direct Anthropic (trusty-agents)

`crates/trusty-agents/src/llm/anthropic_native/mod.rs` +
`llm/adapter/impls.rs:121-144` + `llm/http.rs:379` implement the native
`/v1/messages` path: base `https://api.anthropic.com/v1`, header `x-api-key`,
`anthropic-version: 2023-06-01`, key from `ANTHROPIC_API_KEY`
(`llm/credentials.rs:74,99`). This supplies the third provider the SM needs.

### 2.4 Reuse target — PM instruction composition

`crates/trusty-mpm/src/core/instruction_pipeline.rs` +
`instruction_overrides.rs` + `src/assets/instructions/*.md`:

- Assets embedded via `include_str!`: `PM_INSTRUCTIONS.md`, `WORKFLOW.md`,
  `AGENT_DELEGATION.md`, `BASE_PM.md` (`instruction_pipeline.rs:42-59`).
- `assemble_system_prompt()` joins them in fixed order with `\n\n---\n\n`,
  BASE_PM last as a non-overridable floor (`instruction_pipeline.rs:71`).
- `resolve_pm_prompt(project_dir)` layers `<project>/.trusty-mpm/` overrides onto
  bundled defaults, always appending BASE_PM last (`instruction_overrides.rs:132`).
- Delivered to the spawned session via
  `write_prompt_file` + `build_claude_command` →
  `claude --append-system-prompt-file <tmp>`, started in tmux
  (`core/model_inject.rs:30,69-76`, `bin/tm/commands/launch.rs:127-156`).
- PM content structure: Identity (delegate-only) → canonical Prohibitions table
  → strict Allowlist → workflow phases → BLOCKING verification gates → forbidden
  phrases → BASE_PM floor. The open-mpm `PM_INSTRUCTIONS.md` (version `0014`) is
  the assembled reference.

### 2.5 Current state — memory & palaces

- trusty-mpm is **not** a memory client. It *injects* a stdio MCP server
  (`trusty-memory mcp serve`) + memory hooks into the spawned session's
  `.mcp.json` / `.claude/settings.json` (`core/session_launch/settings.rs:51-100`).
- **No dedicated tm/SM palace, no `palace_create`/`create_palace` call site
  anywhere in trusty-mpm — CONFIRMED.** Writes go to whatever palace the spawned
  session resolves (`trusty-memory` `resolve_palace`,
  `crates/trusty-memory/src/tools/helpers.rs:329-349`).
- Memory verbs live in `crates/trusty-memory/src/tools/` (dispatcher
  `tools/mod.rs:72-100`); the palace engine (incl. `create_palace`) is in
  `crates/trusty-common/src/memory_core/` (`registry.rs:159`).

### 2.6 Current state — session control surface (the SM's hands)

REST in `daemon/api.rs:72-130`; CLI `SessionAction` in `bin/tm/cli.rs:298-374`
dispatched by `bin/tm/commands/session.rs`; MCP `session_list`/`session_status`
(`daemon/mcp_backend.rs`):

| Capability | REST | CLI verb |
|---|---|---|
| list | `GET /sessions` | `List` |
| register / **spawn** (spawn when `workdir` present) | `POST /sessions` (`api.rs:256-339`) | `Start` |
| stop / remove | `DELETE /sessions/{id}` | `Stop` |
| pause | `POST /sessions/{id}/pause` | `Pause` |
| resume | `POST /sessions/{id}/resume` | `Resume` |
| send command (keystrokes) | `POST /sessions/{id}/command` | `Run` |
| capture output / pane | `GET /sessions/{id}/output` (`?compress=`) | `Output` |
| events (SSE/poll) | `GET /sessions/{id}/events[/poll]` | `Events` |
| reap dead | `DELETE /sessions/dead` | `Clean` |
| discover / adopt tmux | `POST /sessions/discover`, `POST /tmux/adopt` | — |

Daemon spawn is **`workdir`-gated** and does **not** handle the trust/permission
dialog (#1269). The CLI-owned launch path (`session.rs:80-113`) creates tmux +
`claude` locally.

### 2.7 Config location

`MpmConfig` in `crates/trusty-mpm/src/core/config.rs:134-151`, loaded from
`~/.trusty-mpm/config.toml` (`MpmConfig::load`, `:170`). Top-level sections today:
`agents`, `models`, `skills`, `pm` (each `#[serde(default)]`). No
`session_manager` section exists — added by this spec (§9, TICKET SM-1).

---

## 3. Role & Prime Directive (workflow instructions — like the PM)

The SM is an **orchestrator that delegates ALL work by launching a t-mpm session
for every unit of work.** It mirrors the PM model (§2.4) one level up: the PM
delegates to agents inside a session; the SM delegates to whole sessions.

### 3.1 Prime directive (NORMATIVE)

> **The session manager does no work itself. Every unit of real work is performed
> by a launched t-mpm session.** The SM's job is to interpret operator intent,
> decompose it into session-sized tasks, launch and observe sessions, verify
> their output, track goals, and report. Producing code, edits, research, file
> reads of project source, builds, tests, or ops directly is a **prohibition
> violation**.

### 3.2 SM Prohibitions table (canonical — single source of truth)

Mirrors the PM Prohibitions table (`PM_INSTRUCTIONS.md:11-28`). Violation =
the SM must instead launch (or route to) a session.

| # | Forbidden for the SM | Must instead |
|---|---|---|
| SP1 | Edit/Write any project file | Launch a session scoped to that edit |
| SP2 | Read project source / deep code analysis | Launch a session (or query trusty-search via a session) |
| SP3 | Run builds, tests, lint, ops, or any non-trivial Bash | Launch a session |
| SP4 | Research / web investigation that becomes deliverable work | Launch a session |
| SP5 | "Quickly" answer a work question from its own model knowledge instead of delegating | Launch a session and report its verified result |
| SP6 | Claim a goal/session is "done" without observed verification evidence | Observe the session + apply the verification gate (§3.5) |
| SP7 | Instruct the operator to run commands the SM should delegate | Launch a session |

No exceptions for "trivial", "it's just one line", or cost-saving arguments —
identical to the PM floor (`BASE_PM.md:9-13`).

### 3.3 SM Allowlist — what the SM MAY do directly

The only directly-permitted SM actions (mirrors the PM Allowlist,
`PM_INSTRUCTIONS.md:30-38`):

1. **Talk to the operator** (the TUI input box; free-text answers, triage, status).
2. **Query its own memory** — `memory_recall` / `memory_recall_deep` against the
   SM palace and (read-only) other palaces it is granted (§11).
3. **Write its own memory** — `memory_remember` / `memory_note` of goals,
   session outcomes, decisions, cross-session knowledge (§11.4).
4. **Drive the session control surface** (§2.6): spawn/list/observe (output +
   events)/pause/resume/stop/command. Observation reads *session panes*, not
   project source — this is allowed (it is the SM's instrument panel).
5. **Track goals** — create/update/close goal records (§12).
6. **Summarize & triage** session state for the operator (§3.6).
7. **Compact its own conversation context** (§10).

Anything that produces a deliverable, mutates a repo, or requires reading project
code → **must be a launched session** (SP1-SP5).

### 3.4 Delegation workflow (the SM loop)

A 6-phase loop, analogous to the PM's 5-phase workflow (`WORKFLOW.md:5-31`):

1. **Intake.** Parse operator intent into a *goal* (§12). Recall relevant memory
   (prior goals, session outcomes, decisions) to inform decomposition.
2. **Decompose.** Break the goal into session-sized tasks. One task → one
   launched session (a goal may fan out to several). Choose project/workdir,
   model tier, and the PM prompt the session will carry.
3. **Launch.** Spawn each session via the control surface (`POST /sessions` with
   `workdir`, or route through the CLI launch path). Record `goal_id → session_id`
   links (§12.3).
4. **Observe.** Poll session output/events; interpret pane state (the
   `session-manager-driver` skill's inference applies — the SM can interpret raw
   panes even without provider inference). Answer session decision prompts by
   sending commands when appropriate.
5. **Verify (BLOCKING gate, §3.5).** Before reporting a goal/task as done, confirm
   acceptance criteria with observed evidence from the session (test output,
   diff, "PR opened", etc.). No evidence → not done.
6. **Report & persist.** Summarize outcome to the operator; update the goal
   status; write the outcome + decisions to the SM palace (§11.4).

### 3.5 Verification gate (BLOCKING)

Mirrors the PM QA Verification Gate (`PM_INSTRUCTIONS.md:180-191`,
`WORKFLOW.md:48-58`). The SM must not claim a goal complete without **observed
evidence from the session(s)**:

| Claim | Required evidence (observed in the session pane / events) |
|---|---|
| "tests pass" | captured test run output with a pass count |
| "PR opened" | the PR URL printed by the session |
| "edit made" | the diff / file-write confirmation in the pane |
| "goal done" | every linked task verified + acceptance criteria met |

**Forbidden SM phrases** (echoing `WORKFLOW.md:60-67`): "should be done",
"looks complete", "probably finished". The SM states the claim **with** the
evidence or states the actual unverified status.

### 3.6 Triage & summarization for the operator

The SM continuously summarizes fleet state for the DOC-13 session list (the
"last summarized message" column, DOC-13 §6) and answers operator triage
questions ("what's blocked?", "which sessions are waiting on me?"). Summaries are
generated by the configured provider; with no provider they fall back to the
deterministic pane-heuristic summary (DOC-13 G5 / §6 degradation).


---

## 4. Custom system prompt

The SM carries its own role-specific system prompt, composed and delivered using
the same machinery as the PM prompt (§2.4): bundled `include_str!` assets under
`src/assets/sm_instructions/`, assembled by an `assemble_sm_prompt()` (mirror of
`assemble_system_prompt`), layered with optional `~/.trusty-mpm/sm/` overrides via
a `resolve_sm_prompt()` (mirror of `resolve_pm_prompt`), and joined with
`\n\n---\n\n` with a non-overridable SM floor appended last.

**Crucial distinction from the PM:** the SM prompt is **not** delivered via
`claude --append-system-prompt-file` (the SM is not a spawned Claude Code session
— it is the daemon-side brain). It is supplied as the **system message** of the
provider request (§5). At runtime the *working prompt* prepends the assembled SM
system prompt; see §10.4 for assembly order.

### 4.1 Asset files (proposed)

| File | Role (mirrors PM asset) |
|---|---|
| `SM_INSTRUCTIONS.md` | Identity + Prohibitions table (§3.2) + Allowlist (§3.3) |
| `SM_WORKFLOW.md` | The 6-phase delegation loop (§3.4) + verification gate (§3.5) |
| `SM_TOOLS.md` | The verbs the SM may call (session control, memory, goals) |
| `BASE_SM.md` | Non-overridable floor: delegate-only identity, no-exceptions rules |

### 4.2 Draft system prompt text (NORMATIVE starting point)

> The wording below is the draft to bundle. It is intentionally PM-shaped.

```markdown
# Session Manager (SM) — trusty-mpm

## Identity
You are the **session manager (SM)**: the orchestrator the operator talks to in
the trusty-mpm coordinator TUI. You manage a fleet of durable, tmux-backed
Claude Code (t-mpm) sessions. You DELEGATE ALL WORK by launching a session for
every unit of work. You never edit code, read project source, run builds/tests,
or do research yourself — you spin up a session that does it, then you observe,
verify, and report.

DEFAULT: delegate by launching a session. There is no "you do it" exception —
the SM has no hands of its own; its hands are the sessions it launches.

## Prohibitions (CANONICAL — single source of truth)
Violating any rule below means: launch (or route to) a session instead.
| #   | Forbidden | Instead |
| SP1 | Edit/Write any project file | Launch a session for the edit |
| SP2 | Read project source / deep analysis | Launch a session |
| SP3 | Builds, tests, lint, ops, non-trivial Bash | Launch a session |
| SP4 | Research that becomes deliverable work | Launch a session |
| SP5 | Answer a work question from your own knowledge | Launch a session, report its verified result |
| SP6 | Claim done without observed evidence | Observe + apply the verification gate |
| SP7 | Tell the operator to run commands you should delegate | Launch a session |
No exceptions for "trivial", "one line", or saving cost/time.

## You MAY do directly (Allowlist)
1. Talk to the operator (answer, triage, status).
2. Recall your memory (SM palace + granted read-only palaces).
3. Remember to your memory (goals, session outcomes, decisions, knowledge).
4. Drive sessions: spawn, list, observe output/events, pause, resume, stop,
   send commands. Observing session PANES is allowed; reading project SOURCE is not.
5. Track goals (create, update, close).
6. Summarize and triage fleet state for the operator.
7. Compact your own conversation context.

## Workflow (your loop, every request)
1. INTAKE — turn operator intent into a goal; recall relevant memory.
2. DECOMPOSE — split the goal into session-sized tasks (1 task → 1 session;
   a goal may fan out to several). Pick project/workdir + model tier per task.
3. LAUNCH — spawn each session; record goal→session links.
4. OBSERVE — poll output/events; interpret panes; answer session decisions.
5. VERIFY (BLOCKING) — before reporting done, confirm acceptance criteria with
   evidence observed in the session (test output, diff, PR URL). No evidence =
   not done.
6. REPORT & PERSIST — summarize to the operator; update goal status; write the
   outcome + decisions to your palace.

## Verification gate (BLOCKING)
Never say "should be done / looks complete / probably finished". State the claim
WITH the evidence, or state the actual unverified status.
| Claim | Evidence required (from the session pane/events) |
| tests pass | captured run output + pass count |
| PR opened | the printed PR URL |
| edit made | the diff / write confirmation |
| goal done | all linked tasks verified + acceptance met |

## Tools available to you
- Session control: spawn(workdir, model, prompt), list, output(id), events(id),
  command(id, text), pause(id), resume(id), stop(id).
- Memory: memory_recall(query), memory_recall_deep(query), memory_remember(text),
  memory_note(text) — scoped to the `session-manager` palace by default.
- Goals: goal_create(desc, acceptance), goal_update(id, status, progress),
  goal_link(id, session_id), goal_close(id, outcome).

## Tone & output
Concise, operator-facing, status-first. Lead with what changed and what's
blocked. When summarizing the fleet, one line per session: ID | crisp status.
Surface decisions the operator must make. Never pad.
```

### 4.3 SM floor (`BASE_SM.md`, non-overridable)

A short floor appended last (mirrors `BASE_PM.md`): re-states the delegate-only
identity, the "no exceptions" rule, and mandates that Trusty tools
(`trusty-memory`, `trusty-search` via a launched session) are preferred over
ad-hoc shell — and that the SM itself never shells out for work (SP3).

---

## 5. Multi-provider inference (config-selectable)

The SM supports three providers, selected by config (§9), reusing existing
abstractions rather than reinventing them.

### 5.1 Providers

| Provider | Reuse target | Auth | Endpoint |
|---|---|---|---|
| `openrouter` | trusty-review `OpenRouterProvider` (`llm/openrouter.rs`) / existing `llm_overseer.rs` | `OPENROUTER_API_KEY` (bearer) | `https://openrouter.ai/api/v1/chat/completions` |
| `bedrock` | trusty-review `BedrockProvider` (`llm/bedrock/mod.rs`), AWS Converse | AWS default credential chain + region (`TRUSTY_AWS_REGION` > `AWS_REGION` > `us-east-1`) | Bedrock Runtime Converse |
| `anthropic` | trusty-agents `anthropic_native` (`llm/anthropic_native/mod.rs`, `llm/http.rs:379`) | `ANTHROPIC_API_KEY` (`x-api-key`, `anthropic-version: 2023-06-01`) | `https://api.anthropic.com/v1/messages` (override via `ANTHROPIC_BASE_URL`) |

### 5.2 Reuse decision (NORMATIVE)

- **D5.1 — Lift trusty-review's `llm` module into a shared crate** (proposed:
  `trusty-common::llm`, or a new `trusty-llm` crate) so both trusty-review and the
  SM consume one prefix-routed, non-streaming, cost/latency-aware `LlmProvider`
  factory. The SM's reasoning is request/response (not token-streamed to a UI),
  so the trusty-review `complete`-style trait fits better than trusty-common's
  streaming `ChatProvider`. *Extraction is its own ticket (SM-1 dependency).*
- **D5.2 — Add an `AnthropicProvider`** to that shared module implementing the
  same `LlmProvider` trait, porting the trusty-agents native `/v1/messages`
  request/response builders (`anthropic_native/mod.rs`) and the `x-api-key` /
  `anthropic-version` headers.
- **D5.3 — Extend prefix routing** so model ids may carry `openrouter/`,
  `bedrock/`, or `anthropic/` prefixes (`resolve_provider_and_model`,
  `llm/mod.rs:202`), with the SM config's `provider` as the default when no prefix
  is present.

### 5.3 Selection, precedence & fallback

- **Explicit config wins.** `[session_manager].inference.provider` selects the
  active provider; `model` selects the model id (a prefix on `model` overrides
  the provider for that call — D5.3).
- **Default provider precedence (when `provider` is unset/`auto`):**
  `anthropic` (if `ANTHROPIC_API_KEY`) → `bedrock` (if AWS creds resolvable) →
  `openrouter` (if `OPENROUTER_API_KEY`) → **none** (degraded mode, G6/§3.6).
- **Fallback chain (optional, `inference.fallback = [...]`):** on a *retryable*
  provider error (use trusty-review's `LlmError` retryable classification,
  `llm/error.rs`) the SM tries the next provider in the chain. Fallbacks are
  off by default to keep behavior deterministic and cost predictable; when on,
  each fallback is logged.
- **Degraded mode (no provider).** The SM still serves: routed `@prefix:`
  commands, session list/management, goal listing, and deterministic pane-based
  summaries. Free-text reasoning returns the DOC-13 "no inference configured"
  notice (matching the current `503` semantics at `coordinator_routes.rs:128-132`,
  but surfaced as a graceful notice in the SM path rather than a hard 503).

### 5.4 Model-tier strategy (per-task model selection) — NORMATIVE

The SM runs **two distinct classes of LLM call**, and they default to **different
model tiers** because their cost/quality profiles differ:

1. **SM chat / orchestration** — the conversational brain that interprets operator
   intent, decomposes goals, and drives the delegation loop (§3.4). The SM
   **largely RELAYS instructions and orchestrates** rather than doing heavy
   reasoning, so it defaults to a **MEDIUM** tier model — **Sonnet**
   (`claude-sonnet-4-6` tier). This is the floor that keeps orchestration crisp
   without paying for a top-tier reasoner.
2. **Session summarization & context compaction** — the per-session
   `last_summary` (the DOC-13 "last summarized message", §3.6) **and** the rolling
   auto-compaction compression calls (§7.3). These are frequent, mechanical
   summarize/compress operations, so they default to an **INEXPENSIVE** tier model
   — **Haiku** — to keep cost low at high call volume.

#### Model-tier table (defaults + rationale)

| LLM task | Default tier | Default model | Rationale |
|---|---|---|---|
| SM chat / orchestration | **MEDIUM (Sonnet)** | `claude-sonnet-4-6` tier | SM relays + orchestrates; needs reliable instruction-following, not top-tier reasoning. |
| Session summarization (`last_summary`) | **INEXPENSIVE (Haiku)** | Haiku tier | Frequent, short, mechanical summaries of session panes; cost-sensitive. |
| Context auto-compaction compression (§7.3) | **INEXPENSIVE (Haiku)** | Haiku tier | Runs ≈ once per 10 rounds; lossless-on-decisions summary, not reasoning; cost-sensitive. |

#### Per-task model overrides (multi-provider) — NORMATIVE

The multi-provider config (openrouter / bedrock / anthropic) **must support
per-task model overrides**, each independently resolvable on **any of the three
providers** via the prefix-routing rule (D5.3). Two distinct model fields are
specified (config in §10):

- **`sm_model`** — the SM chat/orchestration model. Default: Sonnet tier
  (`anthropic/claude-sonnet-4-6`, or the provider-equivalent when `provider` is
  `openrouter`/`bedrock`).
- **`summary_model`** — the model used for **both** per-session `last_summary`
  **and** rolling-compaction compression. Default: Haiku tier
  (`anthropic/claude-haiku-*`, or the provider-equivalent).

Each field may carry an `openrouter/`, `bedrock/`, or `anthropic/` prefix to pin a
specific provider for that task, overriding the active `provider` per call
(D5.3). When a field is empty, it resolves to its tier default on the active
provider. `compaction_model` (§7.3) is retained as a more-specific override that,
when set, supersedes `summary_model` for the compaction call only; when unset, the
compaction call uses `summary_model`.

### 5.5 Cost & determinism

- Temperature default `0.3` for the SM (lower than the current `0.7` overseer
  default — orchestration favors determinism over creativity); configurable.
- The shared `LlmProvider` returns token usage + latency + estimated cost
  (already provided by trusty-review's trait); the SM logs per-call cost and
  exposes a running session-cost total to the TUI (DOC-13 status bar).
- Summarization + compaction calls use the cheaper `summary_model` (Haiku tier,
  §5.4) by default; the more-specific `compaction_model` (§7.3) overrides it for
  the compaction call when set.


---

## 6. Architecture — where the SM lives

### 6.1 Placement

The SM is a **daemon-side service** inside trusty-mpm with an
**interface-agnostic core** (§1A.1). Its **primary transport is a STDIO JSON-RPC
adapter** (the API-first path); it *also* powers the `/coordinator` (and aliased
`/session-manager`) HTTP endpoints, replacing the current `LlmOverseer` brain
(`daemon/llm_overseer.rs`) behind those endpoints. UIs (TUI/Telegram/Slack/Web)
are thin front-ends over the same surface (§1A.3).

```
┌────────────────────────────────────────────────────────────────┐
│ trusty-mpm daemon                                                │
│                                                                  │
│  PRIMARY: claude-mpm / PM ──JSON-RPC/stdio──► SM core (§1A.1)    │
│  ALSO:    DOC-13 TUI / GUI ──HTTP──► /api/v1/sessions/chat       │
│                              (alias /api/v1/session-manager/chat)│
│                                   │                              │
│                                   ▼                              │
│                ┌──────────────────────────────┐                 │
│                │ STDIO JSON-RPC adapter (§1A.1)│  thin mapping   │
│                │  sm.chat / sm.goals.* /       │                 │
│                │  sm.sessions.* / sm.context / │                 │
│                │  sm.health                    │                 │
│                └───────────────┬──────────────┘                 │
│                                ▼                                 │
│                         ┌───────────────────┐                   │
│                         │ SessionManagerAgent│  (this spec)      │
│                         │  - SmContextEngine │  §7               │
│                         │  - SmProviders     │  §5 (tiers §5.4)  │
│                         │  - SmMemory(palace)│  §8               │
│                         │  - SmGoals         │  §9               │
│                         └─────────┬─────────┘                   │
│                 delegates by driving ▼                          │
│            session control surface (api.rs:72-130) §2.6         │
│                                   │                              │
│                                   ▼                              │
│                  tmux-backed Claude Code sessions (t-mpm PM)     │
└────────────────────────────────────────────────────────────────┘
```

### 6.2 Relationship to DOC-13 (TUI) and #1247 (session manager)

- **DOC-13 (TUI)** is the *presentation* of the SM: the input box routes operator
  text to the SM agent; the session list renders the fleet the SM manages
  (including the "last summarized message" the SM produces, §3.6). DOC-13 §0.1
  endpoint contract is preserved by D0.1.
- **The session-manager control surface (#1247 / §2.6)** is the SM's *hands*. The
  SM never bypasses it — it spawns/observes/stops exclusively through those
  REST/CLI verbs, so the TUI and the SM see a consistent fleet.
- **The `session-manager-driver` skill** documents the same spawn→observe→answer
  →stop loop for a *human or external agent*; the SM agent is the *built-in*
  embodiment of that loop, with the added LLM brain (this spec) layered on top.

### 6.3 Stateful vs stateless shift

Today the daemon is stateless about chat (§2.1). The SM introduces **daemon-side
conversation + goal state** (§10 persistence, §12 persistence). This is a
deliberate change: an orchestrator needs continuity. State lives in the SM palace
(durable cross-session knowledge) and a daemon state file (the live rolling
context); see §10.3 and §11.

---

## 7. Infinite context with rolling auto-compaction

### 7.1 Data model

```
SmConversation {
  compressed_context: String,          // running compressed summary of old rounds
  recent_rounds: VecDeque<Round>,      // last N verbatim rounds (N = 10)
  total_rounds: u64,                   // monotonic counter
  token_estimate: usize,               // running estimate for trigger
}
Round { user: String, assistant: String, ts: DateTime, tool_calls: Vec<ToolTrace> }
```

`recent_rounds` is a bounded window of the **last 10 conversation rounds,
verbatim**. `compressed_context` is a growing prose block summarizing everything
older.

### 7.2 Rolling window + compaction trigger

- Each new round (operator message + SM reply, including tool traces) is appended
  to `recent_rounds`.
- **Trigger (whichever first):** (a) `recent_rounds.len() > 10`, OR (b)
  `token_estimate` exceeds `inference.context_token_budget` (default e.g. 24k,
  configurable). Round-count is the primary trigger per the brief; the token
  budget is a safety valve for unusually long rounds.
- On trigger, the **oldest round(s)** are *folded* into `compressed_context`: the
  SM issues a compression call (§7.3) that takes the current
  `compressed_context` + the round(s) being evicted and returns an updated
  `compressed_context`. The evicted rounds are then dropped from `recent_rounds`,
  restoring the window to ≤10.

This yields a **rolling 10-round verbatim window + a monotonically growing
compressed summary** — i.e. effectively infinite context bounded by the
compressed block's size (itself periodically re-compacted, §7.5).

### 7.3 The compression call

- Performed by a **configurable INEXPENSIVE (Haiku-tier) model** — the same
  `summary_model` used for per-session `last_summary` (§5.4). Resolution order:
  `inference.compaction_model` (most-specific override) → `inference.summary_model`
  (Haiku-tier default, e.g. `anthropic/claude-haiku-*` or the provider-equivalent
  such as `openrouter/meta-llama/llama-3.1-8b-instruct:free`) → the active
  provider's model if both are unset.
- Prompt: a fixed compaction instruction ("merge the prior summary and these
  evicted rounds into an updated, faithful, lossless-on-decisions summary;
  preserve goal ids, session ids, decisions, blockers, and open questions; drop
  chit-chat"). Temperature `0.0` for determinism.
- Cost is bounded: one compaction call per overflow event, not per round.

### 7.4 Persistence

- **Live conversation state** (`compressed_context` + `recent_rounds`) persists
  to a **daemon state file**: `~/.trusty-mpm/sm/conversation-<conv_id>.json`
  (atomic write). This survives daemon restart so a conversation resumes intact
  (the connection-safe restart convention, CLAUDE.md #534, applies).
- **Durable distilled knowledge** (goal outcomes, decisions, cross-session
  facts) is *additionally* written to the **SM palace** (§11.4) via
  `memory_remember` — the palace is the long-term store; the state file is the
  hot working buffer. The two are complementary: the state file is per-conversation
  and ephemeral-ish; the palace is cross-conversation and permanent.

### 7.5 Working-prompt assembly (per request)

The provider request messages are assembled in this order:

1. **System message** = assembled SM system prompt (§4) + non-overridable floor.
2. **Compressed context block** = `compressed_context` (as a system/context
   message: "Earlier in this conversation: …").
3. **Memory recall block** = top-k `memory_recall` hits from the SM palace
   relevant to the current operator message (§11.3) — injected as context.
4. **Recent rounds** = the ≤10 verbatim `recent_rounds` as alternating
   user/assistant turns.
5. **Current operator message** = the new user turn.

### 7.6 Determinism & cost notes

- Reasoning temperature `0.3` (§5.4); compaction temperature `0.0` (§7.3).
- Compaction frequency ≈ once per 10 rounds → amortized cost is low.
- The compressed block is itself re-compacted when it exceeds a size cap
  (`inference.compressed_context_max_tokens`) by re-summarizing it (a "compact the
  summary" pass), preventing unbounded growth.
- All compaction calls are logged with token usage so cost is auditable.


---

## 8. Memory access + dedicated SM palace

### 8.1 The gap this fixes

Investigation confirmed (§2.5) that **no dedicated trusty-mpm/SM palace exists
and trusty-mpm never creates one** — spawned sessions write to whatever palace
they resolve. The SM needs its **own** durable, cross-session store, distinct
from project palaces and the user's personal palace.

### 8.2 Palace identity & idempotent creation (NORMATIVE)

- **D8.1 — The SM owns a dedicated palace named `session-manager`** (rationale:
  the `tm` alias already means *trusty-memory*, so `session-manager` is
  unambiguous; configurable via `[session_manager].memory.palace`).
- **D8.2 — The SM creates the palace idempotently at startup.** On daemon start,
  the SM calls `palace_create` for `session-manager` if absent (the engine's
  `create_palace`, `trusty-common/src/memory_core/registry.rs:159`; verb
  `palace_create`, `trusty-memory/src/tools/definitions.rs:159`). Creation is
  idempotent: existing → no-op.
- **D8.3 — How the SM reaches trusty-memory.** Two options, decided in the
  implementation ticket (SM-4):
  - **(a) Direct lib call** to `trusty-common::memory_core` (add a dependency on
    the memory-core feature). Lowest latency, no extra process; the SM is in-daemon
    so this is natural.
  - **(b) MCP client** to a `trusty-memory` stdio/HTTP server (the pattern the
    spawned sessions use).
  - **Recommendation:** **(a) direct lib** for the SM's own palace I/O (the SM is
    Rust daemon code, not a Claude session), keeping MCP for the spawned sessions.
    This is a notable new dependency for trusty-mpm (today it depends only on
    trusty-common; adding the `memory-core` feature is in-family).

### 8.3 Memory recall feeds the working context

On each operator message the SM runs a scoped `memory_recall` /
`memory_recall_deep` against the `session-manager` palace (and any granted
read-only palaces) for context relevant to the message/goal, and injects the
top-k hits into the working prompt (§7.5 step 3). This is how prior goals,
session outcomes, and decisions inform new decomposition.

### 8.4 What the SM stores (write policy)

The SM writes to its palace at phase 6 of the loop (§3.4) and at notable events:

| Stored item | Verb | When |
|---|---|---|
| Goal records (created/updated/closed) | `memory_remember` (structured) | on goal create/update/close (§12) |
| Session outcomes (id, goal, result, evidence) | `memory_remember` | on session verify/stop |
| Decisions ("chose Bedrock for cost", "split goal X into 3 sessions") | `memory_note` | as made |
| Cross-session knowledge ("project Y needs SKIP_UI_BUILD") | `memory_remember` | on discovery |

Write scope is the `session-manager` palace by default; the SM never writes to
project or personal palaces (read-only access there if granted).

### 8.5 Relationship to per-project palace work (out of scope here)

Ensuring *spawned sessions* each get an appropriate project palace is **separate
work** (noted in §13 / risks). This spec only mandates the SM's own palace.

---

## 9. Goal tracking

### 9.1 Goal model

```
Goal {
  id: GoalId,                  // stable, e.g. "g-<short-uuid>"
  description: String,         // operator intent, normalized
  status: GoalStatus,          // Pending | InProgress | Blocked | Done | Abandoned
  acceptance: Vec<String>,     // acceptance criteria (testable)
  sessions: Vec<SessionLink>,  // linked t-mpm sessions
  progress: u8,                // 0..100 (derived from linked-session verification)
  created: DateTime, updated: DateTime,
  notes: Vec<String>,          // decisions, blockers
}
SessionLink { session_id, task: String, state: SessionTaskState, evidence: Option<String> }
```

### 9.2 Lifecycle

- **Create.** From operator intent at intake (§3.4 phase 1). The SM normalizes
  the goal and proposes acceptance criteria (confirmed with the operator when
  ambiguous).
- **Decompose & link.** Each session-sized task launched (§3.4 phase 3) is
  appended to `sessions` with `state = Launched`.
- **Update.** As sessions are observed/verified (phases 4-5), `SessionLink.state`
  and `evidence` update; `progress` is recomputed (e.g. fraction of linked tasks
  `Verified`).
- **Surface.** Goals appear in the TUI (a `/goals` view or a status line; DOC-13
  cross-ref) and inform the SM's reasoning via recall.
- **Close.** When all linked tasks are `Verified` and acceptance is met, the SM
  applies the verification gate (§3.5) and sets `Done` with a closing outcome
  note; otherwise `Blocked`/`Abandoned` with a reason.

### 9.3 Goal → sessions mapping

One goal maps to **one-or-many** launched sessions (fan-out). A session belongs
to exactly one goal (its task). The SM maintains the `goal_id ↔ session_id`
index; the session control surface's `session_id` (from `POST /sessions`) is the
join key.

### 9.4 Persistence

- **Durable:** goals are written to the SM palace (§8.4) as structured
  `memory_remember` entries (queryable across conversations and daemon restarts).
- **Hot:** the active goal set is mirrored in daemon state
  (`~/.trusty-mpm/sm/goals.json`, atomic) for fast TUI rendering without a memory
  round-trip on every poll. The palace is the source of truth; the state file is
  a cache rebuilt from the palace on startup.

---

## 10. Config schema (`[session_manager]` in `MpmConfig`)

Added to `MpmConfig` (`core/config.rs:134-151`) as a new
`#[serde(default)] pub session_manager: SessionManagerConfig`. File:
`~/.trusty-mpm/config.toml`.

```toml
[session_manager]
enabled = true                      # opt-in; false → legacy LlmOverseer/coordinator behavior

[session_manager.inference]
provider = "auto"                   # "auto" | "openrouter" | "bedrock" | "anthropic"

# Per-task model tiers (§5.4). Each may carry an openrouter/ bedrock/ anthropic/
# prefix to pin a provider for that task (D5.3); empty → tier default on the
# active provider.
sm_model = "anthropic/claude-sonnet-4-6"  # SM chat/orchestration — MEDIUM (Sonnet) tier
summary_model = "anthropic/claude-haiku"  # session last_summary + compaction — INEXPENSIVE (Haiku) tier

# Deprecated alias: `model` (kept for back-compat) is interpreted as `sm_model`
# when `sm_model` is unset.
model = "anthropic/claude-sonnet-4-6"     # legacy alias for sm_model

fallback = []                       # e.g. ["openrouter", "bedrock"]; [] = no fallback
temperature = 0.3
context_token_budget = 24000        # compaction safety-valve trigger
compaction_model = ""               # "" → use summary_model (Haiku); else explicit (prefixed) id overrides for compaction only
compressed_context_max_tokens = 4000

# provider auth is resolved from env (not stored in config):
#   openrouter → OPENROUTER_API_KEY
#   bedrock    → AWS default credential chain + TRUSTY_AWS_REGION/AWS_REGION
#   anthropic  → ANTHROPIC_API_KEY (+ optional ANTHROPIC_BASE_URL)

[session_manager.memory]
palace = "session-manager"          # dedicated SM palace name
recall_top_k = 6                    # hits injected into the working prompt

[session_manager.rounds]
window = 10                         # verbatim rolling window size
```

`SessionManagerConfig` derives `Debug, Clone, PartialEq, Eq, Serialize,
Deserialize, Default`; every field `#[serde(default)]` so partial config files
parse. The `config_valid_parsed` test (`config.rs:334`) is extended to cover it.


---

## 11. Dependencies & risks

| Item | Type | Notes / mitigation |
|---|---|---|
| Provider extraction (trusty-review `llm` → shared crate) | Dependency | SM-2 depends on it; until extracted, the SM could duplicate the module (worse). Prefer extraction. |
| Provider credentials | Risk | Three auth surfaces (OpenRouter key / AWS chain / Anthropic key). Degraded mode (§5.3) keeps the SM useful without any. |
| Bedrock cargo feature | Risk | trusty-common gates Bedrock behind a feature; pulling it into trusty-mpm adds AWS SDK weight + MSRV pressure (aws-smithy drives MSRV 1.91). Keep behind a feature flag. |
| Compaction cost & quality | Risk | A bad compaction loses decisions/goal links. Mitigate: temp 0.0, fixed faithful-summary prompt, preserve ids verbatim, log every compaction (§7.3/§7.6). |
| Trust/headless spawn (#1269) | Blocker | The SM delegates by spawning sessions; daemon-side spawn does not auto-accept the trust dialog. Until #1269 lands, the SM may need CLI-owned launch or operator confirmation. Surface clearly. |
| Per-project palace for spawned sessions | Adjacent work | Out of scope (§8.5) but related; the SM palace work (SM-4) establishes the idempotent-create pattern others can reuse. |
| memory-core dependency for trusty-mpm | Dependency | New dep (direct lib path, D8.3a). In-family (trusty-common) but expands the build. |
| Daemon-side conversation state | Risk | New durable state (§6.3); must use atomic writes + graceful restart (CLAUDE.md #534) to avoid corruption. |
| Cost transparency | Risk | Multi-provider + compaction makes cost opaque; mitigate with per-call usage logging + a running TUI cost total (§5.4). |

---

## 12. Out of scope

- Rust implementation of anything in this spec (this is a spec-only PR).
- The TUI rendering and slash-command modals (DOC-13 owns that). The TUI itself
  is the **UAT UI built after the stdio API is validated** (§1A.2) — out of scope
  for the initial API-first build (epic #1272).
- **Telegram / Slack / Web (console) UIs** — explicitly **deferred** future UIs
  (§1A.3); each is its own future ticket (§14 FUTURE group). The Web UI lives in
  `trusty-console` / `trusty-mpm-gui`.
- Trust-dialog / headless-spawn auto-accept (#1269) — a blocker, not in scope.
- Per-project palace creation for *spawned sessions* (§8.5).
- Retiring the `/api/v1/coordinator/*` paths (alias kept per D0.1).
- Streaming token output to the TUI (the SM is request/response, §5.2).

---

## 13. Open questions

1. **Provider trait source.** Extract trusty-review's `llm` into
   `trusty-common::llm` vs a new `trusty-llm` crate vs adopt trusty-common's
   streaming `ChatProvider` (and add a non-streaming adapter)? (§5.2)
2. **Memory access mechanism.** Direct `memory-core` lib (D8.3a, recommended) vs
   MCP client (D8.3b)? Confirm the dependency/feature cost is acceptable.
3. **Where does SM state live across daemon restarts** — is the daemon state file
   (§7.4) sufficient, or should the entire live conversation also be palace-backed
   for multi-host continuity?
4. **Goal acceptance authoring** — does the SM auto-propose acceptance criteria,
   or always require operator confirmation? (§9.2)
5. **Concurrency limits** — should the SM cap concurrent launched sessions
   (per goal / global), and is that config or policy?
6. **Default model ids** per provider — pin specific ids now or resolve via a
   tier alias (cf. `ModelsConfig.tiers`, `config.rs:69`)?
7. **Compaction provider independence** — should compaction always use the
   cheapest available provider regardless of the reasoning provider? (§7.3)
8. **Relationship to the `pm` model selection** — should the SM reuse
   `ModelsConfig` tiers for the *spawned sessions'* models, or own that choice?

---

## 14. EPIC & ticket breakdown

> **EPIC — Session Manager (SM) agent (DOC-14 / `SPEC-SM-AGENT-01`).**
> Build the daemon-side LLM orchestrator behind the coordinator/session-manager
> endpoints — **API-first**: multi-provider inference with per-task model tiers
> (Sonnet orchestration / Haiku summarization), delegate-all-via-session workflow
> + system prompt, rolling auto-compaction context, a dedicated memory palace,
> goal tracking, and a **STDIO JSON-RPC adapter** so the SM can be driven and
> tested headlessly before any UI. Builds on DOC-13 (PR #1271) and the SM TUI epic
> (#1272). Ships as small, independently mergeable child tickets.

### Build order at a glance (NORMATIVE)

**NOW set (API-first, this epic — build in this order):**
SM-1 (config) → SM-2 (providers + model tiers) + SM-3 (system prompt) + SM-4
(memory/palace) → SM-5 (compaction) → SM-6 (goals) → SM-7 (endpoint wiring) →
**SM-STDIO (JSON-RPC stdio adapter — the headless drive/test surface)** → SM-8
(SM↔session delegation). **All SM functionality is validated over stdio
(SM-STDIO) before any UI is built (§1A.2).**

**FUTURE set (NOT for now — each its own future ticket, §14 FUTURE):**
**TUI (epic #1272)** — the UAT UI built *after* stdio works → **SM Telegram bot
UI** → **SM Slack bot UI** → **SM Web (console) UI**. All are thin front-ends over
the SM core + the same JSON-RPC/HTTP surface (§1A.3).

Child tickets (small, independently shippable):

### SM-1 — `[session_manager]` config section + SM scaffolding
- **Scope:** Add `SessionManagerConfig` (§10) to `MpmConfig` (`core/config.rs`);
  define the `SessionManagerAgent` service skeleton (no inference yet) wired so
  `enabled=false` preserves today's `LlmOverseer` behavior. Extend
  `config_valid_parsed` test.
- **Deps:** none.
- **Acceptance:** config parses (full + partial); `enabled=false` is a no-op vs
  current behavior; clippy/fmt/tests green; SLOC cap respected.

### SM-2 — Multi-provider inference abstraction + per-task model tiers
- **Scope:** Extract trusty-review's `llm` module into a shared location
  (§5.2 D5.1); add `AnthropicProvider` (D5.2) porting trusty-agents'
  `anthropic_native`; extend prefix routing to `anthropic/` (D5.3); implement
  provider selection + precedence + optional fallback (§5.3) and cost/usage
  logging (§5.5). **Implement per-task model tiers (§5.4):** an **`sm_model`**
  defaulting to the **Sonnet** tier (`anthropic/claude-sonnet-4-6`) for
  chat/orchestration, and a **`summary_model`** defaulting to the **Haiku** tier
  for session summarization + compaction; each independently resolvable on any of
  the 3 providers via the `openrouter/`/`bedrock/`/`anthropic/` prefix.
  `compaction_model` (when set) overrides `summary_model` for the compaction call
  only; legacy `model` aliases `sm_model` when `sm_model` is unset.
- **Deps:** SM-1.
- **Acceptance:** unit tests for `resolve_provider_and_model` incl. `anthropic/`;
  `sm_model` resolves to Sonnet-tier and `summary_model` to Haiku-tier by default;
  each per-task model resolvable on each of the 3 providers via prefix;
  `compaction_model` override precedence tested; a `complete()` round-trips against
  each provider (mocked); precedence + degraded mode covered; trusty-review still
  compiles against the extracted module.

### SM-3 — SM system prompt + workflow instructions (delegate-all-via-session)
- **Scope:** Add `src/assets/sm_instructions/*.md` (§4.1) with the draft text
  (§4.2/§4.3); implement `assemble_sm_prompt()` + `resolve_sm_prompt()` mirroring
  the PM pipeline (§2.4); supply it as the provider system message.
- **Deps:** SM-1.
- **Acceptance:** assembled prompt contains the Prohibitions table, Allowlist,
  6-phase loop, and verification gate; override layering + non-overridable floor
  tested (mirror `instruction_overrides` tests).

### SM-4 — Dedicated SM palace + memory recall/remember integration
- **Scope:** Idempotent `palace_create("session-manager")` at startup (§8.2);
  wire `memory_recall`/`memory_remember`/`memory_note` (direct memory-core lib,
  D8.3a); inject recall hits into the working prompt (§8.3/§7.5); write policy
  (§8.4).
- **Deps:** SM-1.
- **Acceptance:** palace created idempotently (create twice = one palace); recall
  returns injected context; remember/note persist and survive restart; SM never
  writes to non-SM palaces (tested).

### SM-5 — Rolling auto-compaction context engine
- **Scope:** `SmContextEngine` with the data model (§7.1), 10-round window +
  trigger (§7.2), compaction call (§7.3), state-file persistence (§7.4),
  working-prompt assembly (§7.5), summary re-compaction (§7.6).
- **Deps:** SM-2 (needs a provider for compaction).
- **Acceptance:** window stays ≤10; eviction folds into `compressed_context`;
  ids/goal links preserved across compaction (golden test); state survives daemon
  restart; compaction cost logged.

### SM-6 — Goal-tracking model + persistence
- **Scope:** `Goal`/`SessionLink` model (§9.1); lifecycle create/decompose/update/
  surface/close (§9.2); goal↔session index (§9.3); dual persistence — palace
  (truth) + `goals.json` cache (§9.4).
- **Deps:** SM-4.
- **Acceptance:** goal created from intent; linking a session updates progress;
  verification gate blocks `Done` without evidence; goals rebuild from palace on
  startup; cache matches palace.

### SM-7 — Wire SM into the coordinator/session-manager chat endpoint
- **Scope:** Route `/api/v1/coordinator/chat` (and new `/api/v1/session-manager/*`
  aliases, D0.1) + `tm sm` CLI alias (D0.2) to `SessionManagerAgent`, superseding
  `LlmOverseer` when `enabled=true`; degraded-mode notice (§5.3); per-call cost in
  the response for the TUI status bar (DOC-13).
- **Deps:** SM-2, SM-3, SM-5.
- **Acceptance:** chat goes through the SM (system prompt + compressed context +
  recall + recent rounds, §7.5); `enabled=false` falls back to `LlmOverseer`;
  alias endpoints/CLI resolve to the same handler; DOC-13 TUI unaffected.

### SM-STDIO — SM JSON-RPC stdio adapter (headless drive/test surface)
- **Scope:** Add a **direct JSON-RPC 2.0 over STDIO** adapter (the **primary**
  transport, §1A.1) exposing the first-cut method surface: `sm.chat`,
  `sm.goals.list/create/update`, `sm.sessions.launch/list/get/stop/resume/kill/send`,
  `sm.context.get`, `sm.health`. The adapter is a **thin mapping** onto the
  existing session-manager API (§2.6) + the SM agent loop (§3.4) — no business
  logic in transport. Newline-delimited JSON-RPC on stdout, logs to stderr only
  (mirrors trusty-* `serve --stdio`). This is the surface a parent
  `claude-mpm`/PM drives to validate **all** SM functionality headlessly before
  any UI (test topology in §1A.2).
- **Deps:** SM core + provider (SM-2) + system prompt (SM-3); ideally also SM-5
  (`sm.context.get`) and SM-6 (`sm.goals.*`) for the full surface, but `sm.chat`/
  `sm.sessions.*`/`sm.health` are landable earlier.
- **Acceptance:** each method round-trips over stdio (JSON-RPC 2.0 framing; stdout
  clean, logs on stderr); `sm.chat` drives a full SM turn; `sm.sessions.launch`
  spawns and links a session; `sm.context.get` returns the rolling context state;
  `sm.health` reports provider + degraded + model tiers; a scripted parent driver
  exercises chat→launch→observe→verify→goal-close end-to-end over stdio (the
  §1A.2 sequence, step 1).

### SM-8 — SM ↔ session-manager delegation (launch/observe for all work)
- **Scope:** Implement the delegation loop (§3.4): the SM spawns sessions via the
  control surface (§2.6), observes output/events, answers session decisions,
  applies the verification gate (§3.5), and reports; enforce the Prohibitions
  (§3.2) so the SM never does work directly.
- **Deps:** SM-6, SM-7, **SM-STDIO** (the delegation loop is first validated over
  stdio). (Soft-blocked by #1269 for headless spawn — gate behind CLI-owned
  launch / operator confirm until #1269 lands.)
- **Acceptance:** an operator goal results in ≥1 launched session linked to the
  goal; observation drives goal progress; a goal cannot be reported done without
  observed evidence; SM attempts at direct work are refused/redirected to a
  session (tested); the full loop is exercisable over the SM-STDIO surface.

---

## 14A. FUTURE ticket group (NOT for now — deferred UIs)

> These tickets are **explicitly out of scope for the initial API-first build**
> and are listed here so they can be filed as separate future issues. Each is a
> **thin front-end over the SM core + the same JSON-RPC/HTTP surface** (§1A.3).
> They are built **after** the NOW set above, and only after **all SM
> functionality is validated over stdio** (SM-STDIO, §1A.2).

### FUTURE: SM TUI (UAT UI) — epic #1272
- **Scope:** The coordinator/SM TUI (DOC-13 / PR #1271 / epic **#1272**) as the
  **UAT UI**, built **after** the stdio API is validated. **Tested via the `tmux`
  agent** (the `session-manager-driver` / tmux harness) driving the TUI panes.
- **Deps:** SM-STDIO (stdio surface validated first), SM-7 (endpoint wiring).
- **Acceptance:** operator drives the full SM loop from the TUI; tmux-agent UAT
  pass covers chat, goal surfacing, and session observation.

### FUTURE: SM Telegram bot UI
- **Scope:** A Telegram bot front-end over the SM core (chat + status + triage)
  speaking the JSON-RPC/HTTP surface; no SM core changes.
- **Deps:** SM-STDIO / SM-7 (a stable SM surface).
- **Acceptance:** operator can chat with the SM and receive fleet status over
  Telegram; all calls go through the existing SM surface.

### FUTURE: SM Slack bot UI
- **Scope:** A Slack bot front-end over the SM core (chat + status + triage)
  speaking the JSON-RPC/HTTP surface; no SM core changes.
- **Deps:** SM-STDIO / SM-7.
- **Acceptance:** operator can chat with the SM and receive fleet status over
  Slack; all calls go through the existing SM surface.

### FUTURE: SM Web (console) UI
- **Scope:** A Web/console front-end over the SM core (chat + goals + sessions),
  living in `trusty-console` / `trusty-mpm-gui`, speaking the JSON-RPC/HTTP
  surface; no SM core changes.
- **Deps:** SM-STDIO / SM-7.
- **Acceptance:** operator can drive the SM (chat, goals, sessions) from the web
  console; all calls go through the existing SM surface.

---

## 15. Change log

- **2026-06-15** — Initial draft (DOC-14). Investigation of current coordinator
  inference, trusty-review provider abstraction, PM instruction composition, and
  memory/palace state; spec authored; EPIC + 8 child tickets defined.
- **2026-06-15** — Amendment (DOC-14). Added **model-tier strategy** (§5.4):
  Sonnet-tier `sm_model` for chat/orchestration, Haiku-tier `summary_model` for
  session summarization + compaction, with per-task overrides resolvable on any of
  the 3 providers (config §10; compaction §7.3). Added **interface strategy &
  roadmap** (§1A): direct **JSON-RPC over STDIO** as the primary API-first
  transport with a first-cut method surface (`sm.chat`, `sm.goals.*`,
  `sm.sessions.*`, `sm.context.get`, `sm.health`); explicit **build & test
  sequence** (stdio first → TUI for UAT via the tmux agent → defer Telegram/Slack/
  Web); reflected in the architecture diagram (§6.1) and out-of-scope (§12).
  Reworked the ticket breakdown: NOW-vs-FUTURE build order, added **SM-STDIO**,
  updated **SM-2** for per-task model tiers, added the **FUTURE** ticket group
  (TUI #1272 UAT + Telegram/Slack/Web).
