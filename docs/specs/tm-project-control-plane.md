# DOC-35 — `tm project`: Deterministic Project/Session Control Plane (CLI + Multipane TUI)

**Status:** Draft (naming + scope decisions RESOLVED by owner; three-layer framing + DOC-30
salvage folded in 2026-07-10; implementation not started)
**Subsystem:** trusty-mpm — control plane / CLI / TUI / daemon API
**Owner:** Engineering (trusty-mpm)
**Last-updated:** 2026-07-10
**Spec ID:** `SPEC-PROJCTL-01~draft` … `SPEC-PROJCTL-08~draft` (DOC-35)
**Builds on:** DOC-22 — Multi-Repo Session Routing (`docs/specs/multi-repo-session-routing.md`);
DOC-26 — trusty-mpm alpha-1 unified project/session control plane
(`docs/specs/trusty-mpm-alpha-1-control-plane.md`); DOC-16 — Interactive Sessions TUI
(`docs/specs/sessions-tui-interactive.md`); DOC-30 — Project Manager: Vision & Lifecycle
Orchestrator (`docs/specs/DOC-30-project-manager-vision.md`, SUPERSEDED 2026-07-10, salvage
folded into §10 below).
**Cross-ref:** epic **#2108** (`tm project` — deterministic CLI + multipane TUI project/session
control plane, main entry point, THIS epic); epic **#2109** (`tm manager` — inference/portfolio
layer built ON TOP of this control plane, sequenced after; boundary in §11); issue **#1440**
(channel-agnostic SM proxy, inject+summarize, the layer-2 surface — PR pending); issue **#1878**
(DOC-30 retirement / salvage source for §10); issue **#2081** (project `gh_user`,
CLOSED/shipped); issue **#2082** (JIRA boards to watch, OPEN); epic **#1517** (multi-project
awareness); epic **#1272** (sessions TUI); the tmux-lifecycle "single owning `Session`, no
fire-and-forget" standard (#1452).

> **Scope note.** This is a **design spec**: it proposes the `tm project` command tree, the
> daemon endpoints it consumes, the multipane TUI layout, the deterministic configurator model,
> and — because investigation surfaced a real naming collision — a **naming reconciliation**
> between this epic's `tm project` and three pre-existing "project" surfaces. It states *what*
> should be built and *why*, flags every owner-level fork in the road, and closes with a
> child-issue breakdown. **It carries no Rust changes.**
>
> **Revision note (2026-07-06):** the owner has resolved all six open decisions from §9 v1
> (naming, TUI scope, config UX, entry-point gating, and the `jira_boards` placeholder). §2, §5,
> §6, §7, §8, and §9 below reflect the RESOLVED decisions; §2.2's rejected alternatives are kept
> for record but no longer represent an open choice. **Naming headline:** the CLI top-level noun
> for this epic's project surface is the **plural `tm projects`** (registry B-backed, net-new);
> today's registry-A `tm project init/list/info` (singular, unchanged backing) is **deprecated**,
> not merged, and continues to work with a deprecation notice. Session lifecycle verbs are
> promoted to a **sibling top-level plural `tm sessions`**, with today's singular `tm session`
> kept as a deprecated alias — `tm projects` nests sessions **read-only** in its TUI/output, but
> the mutating session verbs live at `tm sessions`, not under `tm projects`. The child issues in
> §8 have been filed under epic #2108 (see the PR/issue list for numbers).
>
> **Revision note (2026-07-10):** the owner articulated a **three-layer communication model**
> (§1.1, quoted verbatim) that reframes this entire epic as the deterministic substrate for
> **Layer 3**, and retired DOC-30 (issue #1878) in favor of the #2108/#2109 epic split, directing
> that DOC-30's data model and 12 design decisions be folded into this spec. This revision: (a)
> adds §1.1 as the new organizing frame for the whole document, mapping every existing/planned
> surface to a layer; (b) adds §10, carrying forward DOC-30's Deliverable/Milestone data model,
> state machine, tier estimation, `spec_ref` linkage, and session↔deliverable binding, adapted to
> this epic's registry-B identity model; (c) adds §11, an explicit boundary contract with `tm
> manager` (#2109) so the deterministic/inference line stays crisp; (d) adds §12, proposed
> (unfiled) work items for the Deliverable/Milestone layer; (e) adds §13, new open questions for
> the owner arising from this fold-in. §2–§9 (naming, CLI, API, TUI, configurator, entry-point,
> child issues, resolved decisions) are **unchanged** — all cross-references by section number
> from the filed child issues (#2114–#2124) remain valid.

---

## 1. Overview and principles

### 1.1 The three-layer communication model (owner directive, 2026-07-10)

This is the organizing frame for the entire document. Everything below — the CLI tree, the daemon
API, the TUI, the configurator, the Deliverable/Milestone model — is scoped as substrate for one
specific layer of a three-layer model the owner articulated on 2026-07-10. Quoted verbatim:

> Three-layer communication model:
> - Layer 1 DIRECT: user talks directly to a session → drops into tmux. Deterministic. The
>   session manager helps with the connection (`tm ls` picker) but is not in the loop.
> - Layer 2 SESSION MANAGER AS PROXY: an inject/summarize option working across external
>   channels (MCP, Telegram, Slack). A layer on top of direct deterministic session
>   communication. Session-aware, but STILL SINGLE-SESSION-COMMUNICATION FOCUSED.
> - Layer 3 PROJECT MANAGER: the last layer — the user talks to a SINGLE agent that has FULL
>   SCOPE of the user's activities.

`tm project` (epic #2108, this spec) is **the deterministic, no-LLM substrate for Layer 3** — the
data model, control plane, status aggregation, and TUI that a Layer-3 agent needs in order to have
"full scope" without any of that scope-holding itself requiring inference. `tm manager` (epic
#2109) is the inference layer built on top of this substrate — the agent that actually *reasons*
over the full portfolio and talks to the user as a single point of contact. §11 states the
boundary between the two precisely.

**Mapping every existing/planned surface onto a layer:**

| Surface | Layer | Notes |
|---|---|---|
| `tm ls` picker, `tm sessions attach`, direct tmux attach | **L1 — Direct** | The user is IN the session's terminal. No proxy in the loop. Unaffected by this spec. |
| `tm sessions <verb>` (list/launch/kill/resume/decommission/activity, §3.2) | **L1 — Direct** (the *deterministic* connection/lifecycle layer L1's picker relies on) | These verbs manage the connection and lifecycle; they do not proxy conversation content. |
| SM proxy inject/summarize (#1440, channel-agnostic, SHIPPED PR #2372 squash 362cb72a) — MCP/Telegram/Slack managed-session HTTP endpoints (focus/unfocus/message/summary routes + ManagedBackend trait), focused-session routing | **L2 — Session Manager as proxy** | Explicitly single-session-focused per the owner's framing, even though it spans external channels. Out of scope for this spec; the HTTP contract is defined in `daemon/managed_routes/mod.rs`. |
| TELUI (epic #1272 — STUI epic; DOC-19 TELUI cites it as feature parent) | **L2** | A channel-specific rendering of the L2 proxy. |
| `tm projects`/`tm sessions` CLI (§3), daemon API (§4), multipane TUI (§5), configurator (§6), Deliverable/Milestone data model (§10) | **L3 substrate — deterministic control plane** | **This spec.** No LLM in the loop anywhere in this column — see §11. |
| `tm manager` (epic #2109) — portfolio inference, agentic oversight, cross-session reasoning, external-channel oversight/notifications | **L3 brain — inference layer** | Consumes this spec's data/API as its deterministic substrate; adds the reasoning this spec explicitly does not do. |

The load-bearing distinction the owner drew is **L2 vs. L3**: L2 is a proxy that is still
fundamentally about **one session at a time** (even when it can reach many sessions serially
through inject/summarize); L3 is a single agent with **full portfolio scope** at once. `tm
project` exists so that when `tm manager` (L3's brain) is built, it inherits a data model and
status surface that already spans the whole portfolio deterministically — it does not have to
invent portfolio-wide state itself, and nothing in L3's substrate silently reintroduces L2's
single-session framing.

**Note on L2 HTTP contract (fix for PR #2372 merged 2026-07-10):** §1.1's mapping previously
referenced #1440 as "PR pending"; it shipped in PR #2372. The L2 HTTP endpoints (#1440's managed-
session routes) are defined in `daemon/managed_routes/mod.rs` (focus, unfocus, message, summary
endpoints + ManagedBackend trait); they are NOT listed in §4 (which scopes only the L3 control-
plane endpoints this epic defines). L3 consumes L2's session status primitives (activity, state)
but does not reimplement L2's routing logic.

### 1.2 What this is

Epic #2108 asks for a **deterministic control plane** for trusty-mpm's projects and sessions:
list every registered project, configure each one, see its sessions nested underneath, see a
live per-session "what's being done" status line, and launch/kill/resume/decommission sessions
— from both a scriptable CLI and a multipane TUI. It is explicitly **not** an LLM/PM surface:

| | Deterministic control plane (`tm project`, this spec) | LLM PM orchestration harness |
|---|---|---|
| Decision-making | None — pure config read/write + lifecycle verbs | Session Manager inference (DOC-14), autonomy tiers, learned auto-answer (DOC-23) |
| Backing | Daemon HTTP API, daemon-owned JSON stores | Daemon + an LLM (OpenRouter/Bedrock/etc., DOC-16 D1) |
| Output | Structured (`--json`) or fixed-format human tables | Natural-language summaries, chat |
| Failure mode | Errors are typed, deterministic, retryable | Ambiguous — may re-prompt, re-classify |

The two are **complementary layers on the same daemon**, not competitors: the control plane is
the pipes and valves; the SM agent (DOC-14), the summarizer (DOC-16 D1), and DOC-30's future
Project Manager reasoning all sit on top of it. This spec governs only the deterministic layer.

### 1.3 Daemon as source of truth

Every verb in this spec — CLI or TUI — is a thin client over the `tm` daemon's HTTP API
(`crates/trusty-mpm/src/daemon/`). No client-side state is authoritative; the CLI/TUI read
`--json` snapshots and re-poll after mutations, mirroring the pattern DOC-16 already established
for the sessions TUI (`tui/coordinator/poll.rs`, timer + immediate re-poll-after-mutation).

### 1.4 "Main entry point once tm is stable"

Today, bare `tm` (no subcommand) dispatches to `commands::guided::run_guided_default`
(`crates/trusty-mpm/src/bin/tm/main.rs:230`, cli.rs doc-comment "the guided default fires
(#1708)") — an **in-project, cwd-scoped** spawn/reconnect flow. It is not a project browser and
has no multi-project view. §7 proposes how `tm projects` (the plural CLI noun this epic
introduces — §2) supersedes this as the landing surface without breaking the existing
guided/first-run flows (`commands::first_run`, `commands::guided*` in
`crates/trusty-mpm/src/bin/tm/commands/`), gated on three explicit conditions (§7, RESOLVED).

---

## 2. Naming reconciliation — RESOLVED (owner, 2026-07-06)

Investigation surfaced **three pre-existing surfaces** that already use the word "project" (one
of them literally the `tm project` CLI verb), plus the existing `tm session` verb family this
epic must coexist with or absorb. Presenting all four together is the point: any naming decision
here has to account for all of them at once, not just `tm project` vs `tm session`.

### 2.1 The four surfaces

| # | Surface | Backing type | Identity | CLI today | Status |
|---|---|---|---|---|---|
| A | **Directory registration** | `core::project::ProjectInfo` (`crates/trusty-mpm/src/core/project.rs`) — `{path, name, registered_at}` | absolute filesystem path | `tm project init/list/info` (`bin/tm/commands/project.rs`) → `POST/GET /projects`, `/projects/current`, `/projects/discover` | **Implemented, in use — now DEPRECATED (§2.2)** |
| B | **NL-routing / session-spawn registry** | `project::Project` (`crates/trusty-mpm/src/project/record.rs`) — `{name, repo_url, default_branch, stack_hint, tags, description, gh_user}` | git `repo_url` | **MCP-only**: `project_register`/`project_get`/`project_list` (`mcp/tools/project.rs`); no CLI, no HTTP route | **Implemented (DOC-22, #1517), MCP-only — becomes the backbone of `tm projects` (§2.2)** |
| C | **Project Manager vision** (DOC-30) | **RETIRED 2026-07-10** — content absorbed into §10 and #2109 | N/A | N/A | **SUPERSEDED — DOC-30 closed (issue #1878); content salvaged to this epic and #2109, no standalone implementation** |
| D | **This epic (#2108)** | Registry B (§2.2) | git `repo_url` | `tm projects <verb>` + sibling `tm sessions <verb>` (§2.2, §3) | **New** |

Additionally, `tm session ...` (`bin/tm/commands/session.rs`, `SessionAction` in `cli.rs:1209`)
already carries **two verb families** in one enum: *local project sessions* (`start`, `stop`,
`list`, `tui`, `clean`, `info`, `instructions`, `events`, `breakers`, `pause`, `resume`, `run`,
`output` — scoped to A's directory registry) and *managed fleet sessions* (`new`, `ls`,
`activity`, `send`, `answer`, `attach`, `decommission`, `delete`, `prune-idle` — scoped to B's
`repo_url` identity via `SessionRecord`). This split is already visible in the source comments at
`cli.rs:1203-1213`.

**Why this matters:** #2108's requirement #5 ("sessions nested under projects") and the fleet
grouping already shipped as `GET /api/v1/sessions/managed/fleet`
(`crates/trusty-mpm/src/daemon/managed_routes/fleet.rs`) both key sessions by **B's `repo_url`
identity**, not A's path identity. A session started from a fresh clone in
`~/trusty-mpm-projects/<owner>/<repo>/<session-id>/` (DOC-26 §14.1) has no meaningful A-path —
only a B-`repo_url`. So the control plane's project identity **must** be B, not A.

### 2.2 RESOLVED reconciliation (owner, 2026-07-06)

The owner resolved this as follows (supersedes the v1 "recommendation" below, which is kept as
§2.3 for record):

1. **Registry B is the identity backbone.** `tm projects` (this epic, plural, net-new top-level
   CLI noun) adopts registry B as its backing store and gains the HTTP surface B has never had
   (today B is MCP-only — §4).
2. **Sessions are promoted to a sibling top-level plural namespace, `tm sessions` — NOT folded
   under `tm project(s) session ...`.** `tm projects` and `tm sessions` are siblings: `tm
   projects` owns project list/register/config/show/status; `tm sessions` owns every session
   lifecycle verb (list, launch, kill, resume, decommission, attach, activity). `tm projects
   show <name>` (and the TUI's Sessions pane, §5) display that project's sessions **read-only**
   by calling the same fleet endpoint (§4) — but the *mutating* verbs (launch/kill/resume/
   decommission/attach) live only at `tm sessions`, addressed via `--project <name>` or a bare
   session id/ordinal, never as `tm projects <verb-that-mutates-a-session>`.
3. **Today's singular `tm session <verb>` becomes a deprecated alias of `tm sessions <verb>`.**
   This reuses the precedent already in the codebase (`cli.rs:1433-1459`:
   `ManagedStop`/`RuntimeStop`/`ManagedResume` are `#[command(hide = true)]` aliases that print a
   deprecation notice before falling through to the current verb) — apply the same pattern one
   level up, at the top-level noun. `tm session ls` keeps working, prints a one-line deprecation
   notice pointing at `tm sessions ls`, and behaves identically. No functional change to either
   verb family carried by the old enum (local-project-session verbs and managed-fleet verbs both
   move under the renamed top-level noun together); only the top-level word changes.
4. **Registry A (`tm project init/list/info`, singular, unchanged path-based backing) is
   DEPRECATED, not merged and not hard-cut.** It keeps working exactly as today, but every
   invocation prints a deprecation notice pointing at the new registry-B-backed equivalents
   (`tm projects register`/`tm projects list`/`tm projects show`). It is removed in a **later**
   release, not this epic's. The local-checkout scaffold behavior it provides
   (`.trusty-mpm/{config.toml,sessions/}`, `scaffold_project_dir` in
   `bin/tm/commands/project.rs:105`) is **carried forward** into the new surface as `tm projects
   config <name> --dir <path>` (§3, §6) — the scaffold logic itself is reused verbatim; only its
   entry point and identity key (registry-B `name` instead of a bare path) change.
5. **DOC-30 is RETIRED (issue #1878, closed 2026-07-10); no namespace reservation.** DOC-30's
   Deliverable/Milestone content has been absorbed into §10 of this spec and the inference layer
   (#2109 `tm manager`). The `tm projects plan ...` sub-namespace is NOT reserved; the
   Deliverable/Milestone CLI lives under `tm projects` alongside the rest of the control plane
   (§10.8), not under a separate `plan` namespace. This simplification reflects the completed
   retirement of DOC-30.

**Net naming result (canonical, use these going forward):**

| Old (this spec, v1 draft) | New (RESOLVED) |
|---|---|
| `tm project list/register/config/sessions/launch/kill/resume/decommission/status/attach` | `tm projects list/register/config/show/status` (project ops only) + `tm sessions list/launch/kill/resume/decommission/attach/activity` (session ops, `--project <name>` scoped) |
| `tm session <verb>` (singular, canonical) | `tm sessions <verb>` (plural, canonical); `tm session <verb>` deprecated alias |
| `tm project init/list/info` (registry A, canonical) | unchanged verbs, now DEPRECATED with a notice; canonical replacement is `tm projects register/list/show` (registry B) |

### 2.3 v1 recommendation and rejected alternatives (historical record)

The v1 draft of this spec recommended keeping `tm session` completely unchanged (pure
porcelain/plumbing split with no rename) and folding registry A directly into `tm project`. The
owner's resolution (§2.2) instead **renames** the session surface to a sibling plural noun and
**deprecates rather than folds** registry A. The alternatives considered and rejected at v1
remain valid context for why a full silent rename (no alias) was avoided:

- *Full silent rename with no alias* — would have broken every doc, skill
  (`tm-session-management`, `tm-session-pause`, `tm-session-resume` in `.claude/skills/`), and
  script referencing `tm session` with no migration path. **Rejected**: §2.2 item 3's
  alias-with-deprecation-notice avoids this while still completing the rename.
- *Merge A and B into one struct* — technically cleaner (one `Project` type, one identity), but
  A's path-identity and B's `repo_url`-identity solve genuinely different problems (a bare local
  directory with no remote is a legitimate A-only case — `derive_name_from_url` returning `None`
  is exactly this, DOC-26 §14.4 "no remote → parent/dir slug"). **Rejected**: §2.2 item 4 keeps A
  as its own (now-deprecated) surface rather than merging data models.

---

## 3. CLI command tree

Per §2.2 (RESOLVED), the surface splits into two **sibling top-level plural nouns** — `tm
projects` (project registry ops) and `tm sessions` (session lifecycle ops) — plus the now-
deprecated singulars (`tm project`, registry A; `tm session`, alias of `tm sessions`). All verbs
are thin HTTP clients (§1.3). Every list/show verb supports `--json` for scripting (matching the
existing convention in `session.rs Ls { json: bool }`, `cli.rs:1360-1364`).

**API-first build ordering.** Every verb below is a client of a daemon endpoint (§4) that must
exist first; §8's child-issue slicing reflects this explicitly (slice 1, the registry-B HTTP
surface, is filed as #2114 and gates slices 2–4; the TUI, slices 5–6, gates on the CLI's endpoints
being live, not the other way around). No slice below ships a CLI verb or TUI pane ahead of the
endpoint it calls.

### 3.1 `tm projects` — project registry (registry B)

```
tm projects list [--json] [--tag <tag>]
    # GET /api/v1/projects  → table: name, repo_url, default_branch, gh_user,
    #   session counts by state (via the sessions endpoint, read-only), last_used_at

tm projects register <name> --repo-url <url> [--default-branch <b>] [--description <s>]
                      [--tags <a,b,c>] [--stack-hint <s>] [--gh-user <login>]
    # POST /api/v1/projects  (idempotent upsert — mirrors project_register MCP tool, §4)

tm projects show <name> [--json]
    # GET /api/v1/projects/{name}  → full config PLUS a READ-ONLY nested sessions
    # listing (via the fleet endpoint, §4) — this is the "sessions nested under
    # projects" requirement satisfied as a VIEW. Mutating a session from here is
    # not supported; the output tells the operator to use `tm sessions <verb>`.

tm projects config <name>
    # GET /api/v1/projects/{name}  → config fields only, human or --json
tm projects config <name> set <field> <value>
tm projects config <name> unset <field>
    # PATCH /api/v1/projects/{name}  — deterministic field=value forms, NOT free text.
    # Fields (v1): default_branch, description, tags (append/remove via --add/--remove),
    #              stack_hint, gh_user (#2081, shipped — validated against `gh auth status`).
    # Field (RESOLVED as an opaque placeholder, concrete shape deferred to #2082):
    #   jira_config — an untyped/opaque blob slot; see §6.
tm projects config <name> --dir <path>
    # Local-checkout scaffold (today's DEPRECATED `tm project init`, carried forward
    # per §2.2 item 4): creates <path>/.trusty-mpm/{config.toml,sessions/} and links
    # the checkout to the registry-B entry `<name>` rather than a bare path.

tm projects status <name> [--json]
    # Rollup: session counts by ManagedSessionState, last activity across sessions,
    # config completeness (gh_user set? jira configured?). New aggregation endpoint (§4).
```

### 3.2 `tm sessions` — session lifecycle (registry B-scoped)

```
tm sessions list [--json] [--project <name>] [--all]
    # GET /api/v1/sessions/managed  (optionally GET .../fleet?project=<name> when
    #   --project is given) — the canonical MUTATING-CAPABLE session list; this is
    #   where `tm sessions kill/resume/...` operators find the id/ordinal to target.

tm sessions launch --project <name> --task "<text>" [--ref <branch>]
                    [--name-hint <hint>] [--runtime claude-code|tcode]
    # POST /api/v1/sessions/managed with repo_url/default_branch resolved FROM the
    # named project registry entry — the operator never re-types the URL.

tm sessions kill <session-id-or-ordinal> [--force]
    # POST /api/v1/sessions/managed/{id}/runtime-stop  (workspace preserved, resumable)
tm sessions resume <session-id-or-ordinal>
    # POST /api/v1/sessions/managed/{id}/resume
tm sessions decommission <session-id-or-ordinal>
    # POST /api/v1/sessions/managed/{id}/decommission  (terminal, tombstoned)
tm sessions attach <session-id-or-ordinal>
    # GET /api/v1/sessions/managed/{id}/attach-cmd → prints the `tmux attach -t ...` command
tm sessions activity <session-id-or-ordinal> [--json]
    # GET /api/v1/sessions/managed/{id}/activity → the per-session status line (§5.4)
```

**Deprecated singulars (kept working, notice-only):**

```
tm project init/list/info ...   # registry A — deprecation notice → use `tm projects register/list/show`
tm session <verb> ...           # alias of `tm sessions <verb>` — deprecation notice, identical behavior
```

**Ordinal addressing.** `<session-id-or-ordinal>` accepts either the full `ManagedSessionId` or
the 1-based ordinal from the most recent `tm sessions list [--project <name>]` — mirroring
DOC-16 §5.2's `/<n>` inline-addressing convention, so operators do not have to copy/paste UUIDs.

---

## 4. Daemon API

Reuse is the default; only registry B's missing HTTP surface and one new aggregation route are
net-new.

| Endpoint | Status | Notes |
|---|---|---|
| `GET /api/v1/projects` | **NEW** | Mirrors `project_list` MCP tool (`mcp/tools/project.rs`) over HTTP so the deterministic CLI/TUI do not need an MCP client. |
| `POST /api/v1/projects` | **NEW** | Mirrors `project_register`; idempotent upsert on `name` (matches `ProjectRegistry::register`, `crates/trusty-mpm/src/project/registry.rs:60`). |
| `GET /api/v1/projects/{name}` | **NEW** | Mirrors `project_get`. |
| `PATCH /api/v1/projects/{name}` | **NEW** | Field-level config update (§3 `config ... set/unset`); validates `gh_user` via the existing `resolve_gh_account_env`/`gh auth status` path (`core/gh_account.rs`) per #2081's "fail loudly" requirement. |
| `GET /api/v1/sessions/managed/fleet?project=<name>` | **EXTEND** | Existing `fleet_by_project_route` (`daemon/managed_routes/fleet.rs:61`) already groups by project; add an optional filter param — no new grouping logic. Consumed by BOTH `tm projects show <name>` (read-only nested view, §3.1) and `tm sessions list --project <name>` (mutating-capable list, §3.2) — same data, two CLI entry points per §2.2 item 2. |
| `POST /api/v1/sessions/managed` | **REUSE** | `tm sessions launch`, pre-filled `repo_url`/`default_branch` from the project record. |
| `POST /api/v1/sessions/managed/{id}/runtime-stop` | **REUSE** | `tm sessions kill` (resumable stop). |
| `POST /api/v1/sessions/managed/{id}/resume` | **REUSE** | `tm sessions resume`. |
| `POST /api/v1/sessions/managed/{id}/decommission` | **REUSE** | `tm sessions decommission` (terminal). |
| `GET /api/v1/sessions/managed/{id}/attach-cmd` | **REUSE** | `tm sessions attach`. |
| `GET /api/v1/sessions/managed/{id}/activity` | **REUSE** | `tm sessions activity` / per-session status line (§5) — already returns `state`, `summary`, `pending_decision`, `raw_pane` (`daemon/managed_routes/activity.rs:44-90`). |
| `GET /api/v1/projects/{name}/status` | **NEW** | `tm projects status <name>` aggregation: session counts by `ManagedSessionState`, most recent `last_activity_at`, config-completeness flags. Thin composition over existing `SessionManager::list()` + `ProjectRegistry::get()` — no new persistence. |

**Everything is deterministic and daemon-hosted**, consistent with #2108's core principle — no
endpoint here invokes an LLM classifier; the optional OpenRouter summarizer (DOC-16 D1,
`activity.rs`'s `classification` field) remains an opt-in overlay the TUI may display but never
depends on.

### 4.1 Status aggregation contract — deterministic-only (design sketch for #2117)

`GET /api/v1/projects/{name}/status` is a **pure rollup**, not an inference call. Its contract:

- **Inputs:** `SessionManager::list()` filtered by the project's `repo_url` (via the fleet
  grouping, §4 row 5) and `ProjectRegistry::get(name)` — both already-persisted, already-computed
  state. No network call to an LLM provider, no OpenRouter dependency, no new persistence.
- **Computation:** counting and max/min over fields that already exist — `ManagedSessionState`
  histogram (`Provisioning`/`Active`/`Stopped`/`Errored`/`Decommissioned` counts), `max(
  last_activity_at)` across the project's sessions, and boolean config-completeness flags
  (`gh_user.is_some()`, `jira_config.is_some()` once #2082/#2122 land). Every field is a pure
  function of already-materialized state — re-running it twice with no state change between calls
  yields byte-identical output.
- **§10 extension (Deliverable/Milestone rollup):** once §10 lands, the same endpoint additionally
  reports a `DeliverableStatus` histogram and `MilestoneStatus` histogram for the project — computed
  the same way, over the same kind of already-persisted state (§10.5), with the same "pure rollup,
  zero inference" contract. A Deliverable's `status` field is set by explicit CLI/API mutation or by
  the deterministic gate-check in §10.3 (itself a poll of CI/test-result booleans, not an LLM call)
  — never by an LLM inferring "this looks done."
- **What this endpoint explicitly does NOT do (ties to §11):** it does not summarize *what* a
  session is doing in prose (that is `activity.rs`'s `summary` field, itself only optionally
  LLM-backed and always with a deterministic fallback per DOC-16 D1); it does not decide whether a
  Deliverable should transition state absent an explicit trigger; it does not reason across
  projects (each call is scoped to one `{name}`). Any of that is `tm manager` (#2109) territory.

---

## 5. Multipane TUI

**RESOLVED (owner, 2026-07-06): full 3-pane + actions bar layout ships as v1 — there is no 2-pane MVP cut.**
Projects, Sessions, and Activity panes, plus a contextual actions bar (a 1-row hint line, not a boxed pane),
all land together in the first TUI release; §8's child-issue breakdown reflects this as a single TUI slice
(still split into a skeleton PR and a live-refresh/activity-wiring PR for reviewability, §8, but both are
required for v1, not sequenced as MVP-then-follow-up). Invocation: `tm projects` with no further arguments
launches this TUI when a TTY is attached (mirroring `tm session tui`'s existing invocation pattern,
`cli.rs:1248` `SessionAction::Tui`); `tm projects list --json` remains the scriptable, non-TUI path.

### 5.1 Pane layout (ASCII mockup)

```
┌─ tm projects ─────────────────────────── v0.x.y · daemon ● http://127.0.0.1:7880 ┐
│ PROJECTS (4)              │ SESSIONS — trusty-tools (3)                          │
│ ▸ trusty-tools      ●3    │  1. ● 4f9c…a1  main       Running tests — 12 passed  │
│   trusty-search     ●1    │  2. ◍ 7b2e…c0  feat/x     Awaiting approval: write…  │
│   genealogy         ○0    │  3. ○ d1a8…ff  fix/y      Idle — last activity 6m    │
│   smarterthings     ●1    │                                                     │
│                            │                                                     │
├────────────────────────────┴─────────────────────────────────────────────────────┤
│ ACTIVITY — session 2 (7b2e…c0)                                                    │
│  state: blocked_on_permission · pending: "write to .github/workflows/ci.yml?"     │
│  proposed default: yes · last 3 lines of raw_pane …                              │
├───────────────────────────────────────────────────────────────────────────────────┤
│ [l] launch  [k] kill  [r] resume  [d] decommission  [a] attach-cmd  [c] config    │
│ [Tab] switch pane  [↑↓] select  [Enter] drill in  [q] quit                        │
└───────────────────────────────────────────────────────────────────────────────────┘
```

Four regions (ratatui vertical+horizontal `Layout`, mirroring the `Constraint` conventions
already used in `tui/coordinator/layout.rs` and `tui/health/render.rs`):

1. **Projects pane** (left column, `Constraint::Percentage(25)`) — one row per registered
   project (registry B), a glyph for aggregate state, and a live session count. `▸` marks focus.
2. **Sessions pane** (right column, `Constraint::Percentage(75)`) — the selected project's
   sessions, numbered (DOC-16 §3.2 convention), sourced from `GET
   .../fleet?project=<name>` (§4). **RESOLVED (#2476):** decommissioned (tombstoned) sessions
   are filtered OUT of this pane and its `(N)` header count on every poll tick — decommissioning
   flips a session record's `state` to `"decommissioned"` in the daemon's store but does not
   delete it, so an unfiltered projection would let a dead row (and an inflated count) persist
   forever. The pane counts *live* sessions; a session later replaced by a new one reusing its
   `name` never ghosts or duplicates, since every tick rebuilds the row list wholesale from the
   daemon's current fleet snapshot rather than diffing against the previous tick's rows. See
   `tui/project_ctl/poll/rows.rs::live_session_rows`.
3. **Activity pane** (bottom, `Constraint::Length(4)`) — the focused session's status line,
   sourced from `GET .../{id}/activity` (§4) — the same fields DOC-16 §6.2 already specifies
   (`state`, `summary`/`last_summary` equivalent, `pending_decision`, `proposed_default`).
4. **Actions/key-hint line** (`Constraint::Length(1)`) — contextual verbs bound to the focused
   pane.

### 5.2 Keybindings

| Key | Context | Action |
|---|---|---|
| `↑`/`↓`, `j`/`k` (as motion) | any pane | move selection within the focused pane |
| `Tab` / `Shift+Tab` | — | cycle pane focus: Projects → Sessions → Activity |
| `Enter` | Projects pane | drill into that project's Sessions pane (auto-focus) |
| `l` | Sessions pane | `launch` — opens a small deterministic form (task, ref, name-hint) → `POST /api/v1/sessions/managed` |
| `k` | Sessions pane, row selected | `kill` (runtime-stop) — confirmation gate if session is Active (mirrors DOC-16 §5.6 active-session confirmation) |
| `r` | Sessions pane, row selected | `resume` |
| `d` | Sessions pane, row selected | `decommission` — confirmation gate (terminal) |
| `a` | Sessions pane, row selected | fetch + display `attach-cmd` (copy-to-clipboard where supported, else print) |
| `c` | Projects pane, row selected | open the config view (§6) for that project |
| `q` / `Ctrl-C` | not in a modal | quit (restore terminal) |
| `Esc` | any modal/form | cancel, return to prior pane |

### 5.3 Live-refresh model

Timer-poll at a configurable `--interval-ms` (default 1500ms, matching DOC-16 §3.6 and the
existing `tui/coordinator/poll.rs` cadence) **plus** an immediate re-poll after any mutating
action (launch/kill/resume/decommission/config-set) — same "never wait for the next tick after a
mutation" rule DOC-16 §3.6 already establishes. No event-stream/push channel in v1 (DOC-16 §9
already defers this as future work for the sibling sessions TUI; this spec inherits that
deferral rather than re-litigating it).

### 5.4 Per-session "what's being done" status line

Reuses `GET /api/v1/sessions/managed/{id}/activity` (§4, already shipped) verbatim: `state`
(`working`/`idle`/`blocked_on_permission`/`errored`/`done`/`unknown`), `summary` (human string,
LLM-backed when configured, deterministic fallback otherwise per DOC-16 D1's "clearly marked
fallback" rule), `pending_decision`/`proposed_default`. **No new observability plumbing is
required** — this is the same hook-event/activity-monitor pipeline DOC-16 §6.1 already
specifies; the control-plane TUI is a new *consumer* of an existing *producer*.

---

## 6. Configurator model

**RESOLVED (owner, 2026-07-06): the deterministic config surface ships as BOTH the CLI (`tm
projects config <name> set/unset`, §3.1) AND a TUI config form (§5.2 `c` key) in v1 — same
epic, not a CLI-first-TUI-later split.** Both are thin clients over the same `PATCH
/api/v1/projects/{name}` endpoint (§4), so there is one validation/persistence implementation
behind two front ends.

| Field | Type | Persisted in | Validation |
|---|---|---|---|
| `default_branch` | string | registry B, `~/.trusty-mpm/projects.json` | non-empty |
| `description` | string | registry B | none |
| `tags` | `Vec<String>` | registry B | `--add`/`--remove`, no free-text replace-whole-list footgun |
| `stack_hint` | string | registry B | none (advisory only) |
| `gh_user` | string | registry B (`Project::gh_user`, #2081, already shipped) | must appear in `gh auth status` account list — **fail loudly** per #2081's explicit requirement, never silently accept an unauthenticated login |
| `jira_config` | **opaque placeholder — RESOLVED (owner, 2026-07-06)** | registry B (**new field, reserved now**) | **No concrete shape in this spec.** The owner resolved that this epic reserves a minimal, opaque config slot (e.g. an untyped `Option<serde_json::Value>` or an empty marker struct) purely so the configurator's field table has a place for JIRA config to land, WITHOUT finalizing the schema here. The concrete shape (board id/key, instance URL, auth-token env var name, etc.) is deferred entirely to issue #2082's own design. `tm projects config <name> set jira_config ...` is a **no-op stub** (accepts and stores opaque JSON, does not validate or act on it) until #2082 lands and replaces it with a typed field + real validation. |
| local checkout scaffold (`--dir`) | path | `.trusty-mpm/config.toml` in that checkout (unchanged scaffold from `scaffold_project_dir`) | path must be writable |

**Precedence (unchanged from the existing system, stated for clarity):** a local checkout's
`.trusty-mpm/config.toml` may layer further *local-only* overrides on top of the daemon-owned
registry B entry; the registry entry is the cross-checkout source of truth. This mirrors the
scope model already articulated in issue #920's RFC ("project config overrides system default")
and requires no new precedence machinery.

**Deterministic forms, not free text (explicit requirement):** every mutation is
`set <field> <value>` / `unset <field>` / `--add`/`--remove` for list fields — never a prompt
that accepts arbitrary prose. The TUI's config view (§5.2 `c` key) is a fixed-field form (tab
between fields, edit, confirm), not a chat box — this holds for both the CLI and TUI front ends
per the RESOLVED decision above.

---

## 7. "Main entry point" behavior — RESOLVED (owner, 2026-07-06)

**Current state:** bare `tm` → `commands::guided::run_guided_default` (`bin/tm/main.rs:230`), a
cwd-scoped spawn/reconnect flow with no multi-project awareness. `commands::first_run` handles
true first-time setup.

**RESOLVED transition (two phases; Phase 2 is explicitly gated on three conditions, not a time-
box):**

1. **Phase 1 (opt-in, ships with this epic).** Add a config flag (e.g. `ui.default_landing:
   "guided" | "projects"` in `~/.trusty-mpm/config.toml`, `MpmConfig`, `core/config.rs`), default
   `"guided"` (no behavior change). Operators who want the new dashboard opt in explicitly via
   `tm projects` or the config flag.
2. **Phase 2 (default flip) — BLOCKED until ALL THREE gates hold:**
   - **Gate (a) — shipped and stable.** The 3-pane + actions-bar TUI (§5) and the full `tm projects`/`tm
     sessions` CLI (§3) are shipped, and have not required a stabilization/bugfix pass in the
     immediately preceding release.
   - **Gate (b) — guided/first-run flow preserved, not regressed.** The existing
     `commands::first_run` onboarding and `commands::guided*` cwd-scoped spawn/reconnect flow
     must be reachable **from inside** the `tm projects` experience (e.g. a first-run project
     lands the operator in the same guided setup they get today; `tm projects` with zero
     registered projects does not present an empty, confusing dashboard — it defers to
     `commands::first_run` exactly as bare `tm` does today). New users must not regress relative
     to the current bare-`tm` experience.
   - **Gate (c) — dogfood period.** A dogfood period (duration set by the owner at the time,
     not fixed in this spec) where the opt-in flag (Phase 1) has been used in practice, with
     issues surfaced and fixed, before it becomes the unconditional default.
   Only once (a), (b), and (c) all hold does bare `tm` flip its default: bare `tm` with **zero
   registered projects** still routes to `commands::first_run` (onboarding, unchanged); bare `tm`
   with **≥1 registered project** lands on the `tm projects` 3-pane + actions-bar dashboard (§5) instead of
   the guided cwd-scoped flow. The guided flow remains reachable explicitly (e.g. `tm guided` or
   `tm sessions start`) for the cwd-scoped launch/reconnect use case it already serves well — it
   is not deleted, only demoted from "default when no args" to "explicit verb."

This phasing avoids a hard cutover that breaks the muscle memory of existing `tm` (no args)
users. The three gates are the explicit, documented criteria the owner resolved on (§9) — Phase 2
is not a calendar decision, it is a three-condition checklist that must all be satisfied.

---

## 8. Child-issue breakdown — FILED under epic #2108

Numbered slices, each independently implementable and reviewable, updated to the RESOLVED naming
(§2.2) and scope decisions (§5–§7). Filed as children of #2108; see the issue table below for the
actual issue numbers (filed 2026-07-06).

| # | Slice | Filed as |
|---|---|---|
| 1 | **Registry-B HTTP surface** — `GET/POST /api/v1/projects`, `GET/PATCH /api/v1/projects/{name}` (§4), mirroring the existing MCP tools so the CLI/TUI have a non-MCP path. Gate: HTTP round-trip tests parallel to the existing MCP tool tests. | #2114 |
| 2 | **`tm projects` CLI: list/register/show/status** — §3.1's verbs wired to slice 1 and slice 4. Gate: `cli_parses_projects_*` tests + integration test against a live daemon. | #2115 |
| 3 | **`tm sessions` CLI: promote to sibling top-level plural** — rename `tm session` → `tm sessions` (list/launch/kill/resume/decommission/attach/activity, `--project <name>` scoping, §3.2), with `tm session` retained as a hidden deprecated alias (mirroring the `ManagedStop`/`RuntimeStop` precedent, `cli.rs:1433-1459`). Gate: existing `cli_parses_session_*` tests continue to pass unchanged (alias), new `cli_parses_sessions_*` tests cover the canonical plural form, and a deprecation-notice-printed assertion. | #2116 |
| 4 | **`GET /api/v1/projects/{name}/status` aggregation endpoint** — session-count rollup + config-completeness flags (§4). Gate: unit test with a fixture registry + session set. | #2117 |
| 5 | **Multipane TUI skeleton — 3 panes + actions bar v1 (RESOLVED, no MVP cut)** — Projects/Sessions/Activity panes plus contextual actions bar (§5.1), keyboard nav (§5.2), built as a new module under `crates/trusty-mpm/src/tui/` (e.g. `tui/project_ctl/`, split per the 500-SLOC convention: `layout`, `state`, `poll`, `panes/*`). Gate: manual + the existing TUI test patterns (`tui/coordinator/tests.rs`, `tui/health/tests.rs`) as a template. | #2118 |
| 6 | **TUI live-refresh + activity-pane wiring** — poll cadence, re-poll-after-mutation, activity-pane consumption of `GET .../{id}/activity` (§5.3, §5.4). Gate: TUI state-machine unit tests for refresh timing (mirroring DOC-16 STUI-8's pattern). | #2119 |
| 7 | **Deterministic project configurator — CLI + TUI, RESOLVED same-epic scope** — `tm projects config <name> set/unset` (§3.1) AND the TUI config form (§5.2 `c` key, §6), both thin clients over the same `PATCH /api/v1/projects/{name}`. Gate: one shared validation/persistence test suite exercised from both a CLI integration test and a TUI form unit test. | #2120 |
| 8 | **`gh_user` validation wiring in `PATCH /api/v1/projects/{name}`** — enforce the #2081 fail-loud contract (`gh auth status` check) at config-write time, not just at `gh`-call time. Gate: unit test with a mocked `gh auth status` fixture. | #2121 |
| 9 | **`jira_config` opaque placeholder field reservation** — add an untyped/opaque config slot to `Project`/`ProjectConfig` now (RESOLVED: no concrete schema — deferred to #2082), so the configurator's field table (§6) has a home for it without a breaking schema bump later. Gate: serde round-trip test showing the field defaults to absent/empty, round-trips arbitrary opaque JSON, and is forward-compatible. | #2122 |
| 10 | **Deprecate registry-A `tm project init/list/info`** — RESOLVED: deprecation notice pointing at `tm projects register/list/show`, kept working (not hard-cut), removal deferred to a later release; fold the local-checkout scaffold (`scaffold_project_dir`) forward into `tm projects config <name> --dir <path>` keyed on registry-B `name` (§2.2 item 4, §3.1). Gate: regression test that the old verbs still work and print the notice; new test that `tm projects config --dir` produces the same scaffold output. | #2123 |
| 11 | **Bare-`tm` → `tm projects` entry-point transition** — Phase 1: opt-in `ui.default_landing` config field (§7), wired but defaulting to unchanged (`"guided"`) behavior. Phase 2 (flip the default) is explicitly **BLOCKED** until all three RESOLVED gates hold (§7: shipped+stable, guided/first-run preserved inside `tm projects`, dogfood period) — tracked as a sub-task/checklist on this issue, not started until the owner confirms all three are satisfied. Gate (Phase 1 only, mergeable now): config test + manual verification that `tm` with no flag set behaves identically to today. | #2124 |

---

## 9. Open decisions for the owner — ALL SIX RESOLVED (2026-07-06)

1. **Naming reconciliation (§2.2) — RESOLVED.** Registry B (`project::Project`, `repo_url`-keyed)
   is the identity backbone for the new plural top-level noun **`tm projects`**. Sessions are
   **promoted to a sibling top-level plural noun, `tm sessions`** — NOT nested under `tm
   project(s) session ...`; `tm projects` shows sessions read-only in its output/TUI, but the
   mutating session verbs live only at `tm sessions`. Today's singular `tm session` becomes a
   **deprecated alias** of `tm sessions` (confirmed: yes, alias + deprecation notice, per the
   existing `ManagedStop`/`RuntimeStop` precedent). DOC-30 is RETIRED (issue #1878, closed
   2026-07-10) with its content absorbed into §10 and #2109 — no namespace reservation for `tm
   projects plan ...` (see §2.2 item 5). Filed as #2116 (session rename/alias) and reflected
   throughout §2.2, §3.
2. **Registry A fold-in mechanics (§2.2, §8 slice 10) — RESOLVED.** DEPRECATE, do not merge, do
   not hard-cut: `tm project init/list/info` keeps working, prints a deprecation notice pointing
   at `tm projects register/list/show`, and is removed in a later release. Filed as #2123.
3. **TUI scope/MVP cut (§5) — RESOLVED.** Full 3 panes + actions bar v1 (Projects/Sessions/Activity plus contextual actions bar).
   No 2-pane MVP cut. Filed as #2118 (skeleton) and #2119 (live-refresh + activity wiring).
4. **Deterministic-config UX (§6) — RESOLVED.** BOTH the CLI (`tm projects config set/unset`)
   AND a TUI config form ship in v1, same epic — not CLI-first-TUI-later. Filed as #2120.
5. **Bare-`tm` entry-point timing (§7, §8 slice 11) — RESOLVED.** Not a calendar trigger — gated
   on three explicit conditions, ALL of which must hold before the Phase-2 default flip: (a) the
   3-pane + actions-bar TUI + full CLI are shipped and stable, (b) the guided/first-run flow is preserved
   reachable from inside `tm projects` so new users do not regress, (c) a dogfood period (with
   the opt-in Phase-1 flag) has occurred. Filed as #2124 (Phase 1 actionable now; Phase 2 an
   explicit checklist, not started until the owner confirms all three gates).
6. **`jira_boards` schema shape (§6, §8 slice 9) — RESOLVED.** Reserve a minimal, OPAQUE
   placeholder slot (`jira_config`) now; defer the concrete schema entirely to #2082's own
   design — this epic does not guess at board-id/instance-URL/auth-token conventions. Filed as
   #2122.

All six decisions are reflected in §2, §3, §5, §6, §7 above, and in the filed child issues
(§8, table with issue numbers).

---

## 10. Deliverable/Milestone data model — carried forward from DOC-30 (owner directive, 2026-07-10)

DOC-30 (`docs/specs/DOC-30-project-manager-vision.md`) was retired 2026-07-10 in favor of the
#2108/#2109 epic split (issue #1878). Per the retirement comment, six assets from DOC-30 are
carried forward here rather than lost: the Deliverable/Milestone data model, the S/M/L/XL tier
estimation, the status state machine, `spec_ref` linkage, the one-deliverable-to-many-sessions
binding, and the 12 resolved design rationales. This section adapts them to this epic's registry-B
identity model (§2.2) and the deterministic-only boundary (§1.1, §11) — DOC-30's original draft
predated both and in places assumed inference this spec explicitly excludes (flagged inline below
and carried into §13's open questions).

### 10.1 Why this belongs in `tm project` (L3 substrate), not `tm manager` (L2/L3 brain)

Tracking *that* a Deliverable exists, what tier it is, what state it is in, and which sessions
touched it is bookkeeping — the same class of deterministic fact as "this session is Active" or
"this project has 3 sessions." It belongs in the L3 substrate (§1.1) for the same reason project
and session registries do: `tm manager` (#2109) needs this state to *reason* over, and should not
have to invent or own the ledger it reasons over. **What does NOT belong here:** deciding whether a
Deliverable's *scope* is well-formed, whether an estimate is realistic, or synthesizing a
portfolio narrative from the data — that reasoning is #2109's, per §11.

### 10.2 Deliverable

A **Deliverable** is a discrete unit of work within a project (registry-B `Project`, §2.1 row B —
`repo_url`-keyed, not DOC-30's originally proposed standalone `Project` struct; this epic does not
introduce a second project identity).

```
Deliverable {
  id: DeliverableId (UUID),
  project_name: String,              // registry-B Project.name (repo_url-keyed), not a new ProjectId
  name: String,                      // e.g., "OAuth2 authentication flow"
  description: String,
  kind: DeliverableKind,             // feature | bugfix | refactor | chore | test | docs
  ticket_ref: Option<String>,        // e.g., "#2117" — GitHub issue in THIS epic's convention
  spec_ref: Option<SpecRef>,         // §10.4 — which docs/specs/*.md this implements
  status: DeliverableStatus,         // §10.3 state machine
  estimated_effort: EstimationTier,  // S | M | L | XL (DOC-30 Decision #2, unchanged)
  created_at: DateTime,
  target_date: Option<DateTime>,
}
```

Carried forward unchanged from DOC-30: **tier-based estimation, not hours/ranges** (Decision #2 —
coarse-grained, avoids false precision) and **flat, no recursive sub-tasks** (Decision #3 — add
hierarchy later only if flat proves insufficient).

### 10.3 Status state machine (DOC-30 Decision #9, unchanged)

```
proposed → in-progress → [blocked ↔ in-progress] → complete → delivered/shipped
```

Transition rules (verbatim from DOC-30, still deterministic): `proposed → in-progress` on session
launch against the Deliverable; `in-progress ↔ blocked` is a manual trigger (CLI/TUI action, not
inferred); `in-progress → complete` fires when the **objective gate** — tests green AND
trusty-review APPROVE/CI passing, both already-computed booleans this daemon can poll — passes, OR
via explicit manual confirmation; `complete → delivered/shipped` is always a manual user action.
No skipping `proposed` straight to `complete`/`delivered`. **Milestone status mirrors the rollup of
its contained Deliverables** (§10.5) — same rollup-not-reasoning contract as §4.1.

**Deterministic-boundary note (flagged per §1.1's revision):** the gate check itself ("are tests
green, is trusty-review APPROVE") is a poll of already-computed CI/review state — deterministic,
belongs here. DOC-30's original text also said "otherwise prompt user to confirm" for ambiguous
cases (Decision #8) — the *prompt* is fine (a deterministic escalation, same shape as `activity.rs`
`pending_decision`), but any future *summarization* of "why is this ambiguous" crosses into #2109
territory and must not be added to this endpoint. See §13 Q2.

### 10.4 Spec reference (`spec_ref`, DOC-30 Decision #6, unchanged)

```
SpecRef {
  doc_id: String,       // e.g., "DOC-35"
  file_path: String,    // e.g., "docs/specs/tm-project-control-plane.md"
  implemented: bool,
}
```

**Manual linking only** — the user explicitly sets `spec_ref` when creating/updating a Deliverable.
No auto-scan/heuristic matching (that would itself be an inference feature, out of scope per §11).

### 10.5 Milestone

```
Milestone {
  id: MilestoneId (UUID),
  project_name: String,           // registry-B Project.name
  name: String,                   // e.g., "v1.0 Alpha"
  description: String,
  target_date: DateTime,
  status: MilestoneStatus,        // proposed | in-progress | complete | shipped — rollup of deliverables
  deliverables: Vec<DeliverableId>,
  created_at: DateTime,
}
```

### 10.6 Session ↔ Deliverable binding (DOC-30 Decision #7, unchanged: 1 Deliverable ↔ many Sessions)

No new top-level store. Extend the existing `SessionRecord`
(`crates/trusty-mpm/src/session_manager/record.rs:136`, keyed by `ManagedSessionId`,
`record.rs:28`) with one new optional field:

```
SessionRecord {
  // ...existing fields unchanged (id, repo_url, ref, state, workspace_path, ...)
  deliverable_id: Option<DeliverableId>,   // NEW — which Deliverable this session is working on
}
```

Each session works on exactly ONE Deliverable at a time (or none — `deliverable_id: None` is the
common case for ad-hoc sessions not tracked against a Deliverable); a Deliverable accumulates
multiple sessions over its lifecycle (first attempt, review-fix follow-up, etc.) — not strict 1:1,
not full N:M. `tm sessions launch --deliverable <id>` (extends §3.2) sets this field at launch time;
`tm sessions activity` (§3.2, §5.4) surfaces the bound Deliverable's name/status alongside the
existing per-session status line — this is the "sessions nested under projects... AND under
deliverables" view, satisfied as a read-only annotation, same pattern as §2.2 item 2's project
nesting.

### 10.7 Storage — following the existing tm store pattern

Two new sibling JSON stores next to the existing `~/.trusty-mpm/projects.json` (registry B,
`crates/trusty-mpm/src/project/store.rs`) and `~/.trusty-mpm/session-manager/sessions.json`
(`crates/trusty-mpm/src/session_manager/store.rs`): `~/.trusty-mpm/deliverables.json` and
`~/.trusty-mpm/milestones.json`, same daemon-owned, single-writer, atomic temp-file + rename
pattern (`store.rs`'s existing `save()` implementation, reused verbatim — no new persistence
primitive). `SessionRecord.deliverable_id` (§10.6) is a field addition to the existing
`sessions.json` schema, not a new store.

### 10.8 CLI/API surface sketch (filed as #2378–#2383, see §12)

Mirrors §3's plural-noun, `--json`-everywhere, deterministic-forms conventions:

```
tm projects deliverables list <project> [--json] [--status <s>]
tm projects deliverables add <project> --name "..." --kind feature --estimate S|M|L|XL
                                        [--spec-ref <DOC-N>] [--ticket-ref <#N>]
tm projects deliverables show <project> <deliverable-id> [--json]
                                        # includes bound sessions (§10.6), read-only
tm projects deliverables set-status <project> <deliverable-id> <status>
                                        # explicit transition per §10.3's state machine;
                                        # rejects invalid transitions (e.g. proposed→complete)
tm projects milestones list|add|show <project> ...   # same shape, §10.5
```

`GET /api/v1/projects/{name}/status` (§4.1) gains the `DeliverableStatus`/`MilestoneStatus`
histograms described in §4.1's extension paragraph. No new top-level CLI noun — this nests under
the existing `tm projects` per §2.2's naming resolution, superseding DOC-30's original proposal to
reserve a separate `tm project plan ...` sub-namespace (§2.2 item 5) now that DOC-30 itself is
retired and its content lives directly in this epic. See §13 Q4.

### 10.9 The 12 DOC-30 design rationales — carry-forward status

| # | DOC-30 decision | Carried forward as | Where |
|---|---|---|---|
| 1 | Project ↔ Repo: 1:1 | **Superseded, not carried** — this epic already has a Project identity (registry B, `repo_url`-keyed, §2.1 row B); DOC-30's proposed standalone `Project` struct is dropped, not duplicated. | §2, §10.2 |
| 2 | Estimation tiers S/M/L/XL | Carried forward unchanged | §10.2 |
| 3 | Deliverables flat, no sub-tasks | Carried forward unchanged | §10.2 |
| 4 | CLI-first for MVP | Carried forward — consistent with this whole epic's CLI-before-TUI ordering (§3, §4.1 API-first note) | §10.8 |
| 5 | Both HTTP + MCP exposure | Carried forward — matches §4's existing HTTP-first-then-MCP-mirror pattern (registry B itself is being mirrored MCP→HTTP in §4, same direction) | §10.7, §4 |
| 6 | Manual `spec_ref` linking | Carried forward unchanged | §10.4 |
| 7 | 1 Deliverable ↔ many Sessions | Carried forward unchanged | §10.6 |
| 8 | Tiered completion trigger (gate pass = auto, else prompt) | Carried forward, with the deterministic-boundary flag in §10.3 | §10.3 |
| 9 | Status state machine (linear + blocked branch) | Carried forward unchanged | §10.3 |
| 10 | Autonomy tier binding — PM references, SM/AUTONOMY_POLICY owns | Carried forward as: this epic references but never sets autonomy tiers; unchanged owner (Session Manager) | §11 |
| 11 | DOC-22 resolver as internal input to PM (hierarchical) | **Reassigned to #2109** — routing natural-language intent to a project is inference-adjacent (resolving ambiguity), not this spec's deterministic CLI/API; #2109 is the layer DOC-22's resolver should feed per the three-layer model (§1.1) | §11, §13 Q1 |
| 12 | Delivery verification gates on objective CI/review signals | Carried forward unchanged — this is exactly §10.3's gate check | §10.3 |

---

## 11. Boundary contract with `tm manager` (#2109)

This section exists so the L2/L3 line the owner drew (§1.1) stays crisp as both epics are built in
parallel. It is a contract, not aspiration — anything on the right-hand side below is explicitly
**out of scope for every issue filed under #2108**, including the new proposed work in §12.

| `tm project` (#2108, this spec) WILL | `tm manager` (#2109) territory — this spec WILL NEVER |
|---|---|
| Poll and report already-computed state (session state, CI/review booleans, config flags) — including opt-in-LLM-with-deterministic-fallback fields like activity.rs `summary` (see row 4 below) | Call an LLM/inference provider for new reasoning, new summaries, or decisions not already made elsewhere (surfacing an existing field is not calling an LLM) |
| Store and mutate Deliverable/Milestone records via explicit CLI/API verbs (§10) | Infer a Deliverable's scope, status, or estimate from session content |
| Roll up counts/histograms across sessions within ONE named project (§4.1) | Reason across MULTIPLE projects at once (portfolio-level synthesis) |
| Surface the deterministic `activity.rs` status line (state/summary/pending_decision) as-is | Generate a new prose summary, digest, or narrative not already produced by an existing deterministic or opt-in-LLM-with-fallback field (DOC-16 D1) |
| Execute an explicit, user-triggered transition (`set-status`, launch/kill/resume) | Decide to intervene, escalate, or act on a session without an explicit CLI/API call driving it |
| Expose data over MCP/HTTP for ANY consumer, including #2109, to read | Connect to external channels (Telegram/Slack/MCP-as-a-chat-surface) for two-way portfolio conversation — that is #1440 (L2) or #2109 (L3 brain), never this epic |
| Reference an autonomy tier (T1–T4) for display | Set or evaluate an autonomy tier — Session Manager/AUTONOMY_POLICY.md remains sole owner (DOC-30 Decision #10, unchanged) |

**Test for "which epic does this belong to":** if the behavior is a pure function of already-
materialized state and produces the same output given the same state every time it runs, it is
`tm project`. If it requires an LLM call, cross-project synthesis, or a judgment call not reducible
to an explicit stored flag, it is `tm manager`.

---

## 12. Work items — Deliverable/Milestone layer (FILED 2026-07-10 as #2378–#2383)

The following work items complement §8's filed child issues (#2114–#2124). They were filed under
epic #2108 on 2026-07-10 once §13's owner decisions were resolved (following the same convention
as §8: file after resolving design questions).

| # | Slice | Depends on | Notes |
|---|---|---|---|
| #2378 | Deliverable/Milestone CRUD API — `POST/GET/PATCH /api/v1/projects/{name}/deliverables[/{id}]` and `.../milestones[/{id}]` (§10.2, §10.5) | #2114 (registry-B HTTP surface) | New `deliverables.json`/`milestones.json` stores (§10.7), same atomic-write pattern as `store.rs`. |
| #2379 | `SessionRecord.deliverable_id` field + `tm sessions launch --deliverable <id>` wiring (§10.6) | #2378, existing `session_manager/record.rs` | Additive schema field; existing sessions without it default to `None`. |
| #2380 | Status state-machine enforcement — reject invalid `set-status` transitions per §10.3 | #2378 | Unit tests: one per illegal transition (e.g. `proposed→complete`). |
| #2381 | `tm projects deliverables`/`tm projects milestones` CLI subtree (§10.8) | #2378 | Thin HTTP client, mirrors §3.1's pattern exactly. |
| #2382 | Extend `GET /api/v1/projects/{name}/status` (#2117) with `DeliverableStatus`/`MilestoneStatus` histograms (§4.1 extension) | #2378, #2117 | Additive response fields; existing consumers unaffected. |
| #2383 | TUI: Deliverable glyph in Sessions pane, Deliverable/Milestone view reachable from Projects pane (§10.8's `show`, read-only) | #2378, #2118, #2119 | Extends, does not replace, the existing 3-pane + actions-bar layout (§5.1). |

---

## 13. Resolved decisions for the owner (2026-07-10 amendment)

1. **Does DOC-30 Decision #11 (DOC-22's NL-resolver feeding the portfolio surface) belong to
   #2108 or #2109?** §10.9 row 11 reassigns it to #2109 on the theory that resolving ambiguous
   natural-language intent to a project is inference-adjacent. Confirm, or pull a narrowly-scoped,
   purely-deterministic slice (e.g. exact-name/exact-`repo_url` matching only, no fuzzy NL) into
   #2108 instead.

   **Decision (2026-07-10):** SPLIT. The deterministic resolver primitive (3-strategy, confidence-
   scored, no LLM) remains available as a #2108 substrate API; ACTING on ambiguous/low-confidence
   input (disambiguation choices) belongs to #2109. The L2/L3 line is inference, not lookup.

2. **Where exactly does DOC-30 Decision #8's "prompt user to confirm" sit?** §10.3 keeps the gate
   *check* (poll CI/review booleans) in #2108 but flags that any future summarization of *why* a
   case is ambiguous is #2109's. Confirm this split, or state that the prompt itself (with no
   summarization) is acceptable to ship in #2108 as currently scoped.

   **Decision (2026-07-10):** "Prompt user to confirm" gate: lives in #2108 as a deterministic CLI
   confirmation; #2109 may later auto-answer via policy. Confirmation prompts are not inference.

3. **File §12's WI-12–WI-17 now as children of #2108, or hold until this revision is reviewed?**
   This spec defaults to holding (matching the "resolve then file" convention §8 itself followed).

   **Decision (2026-07-10):** FILE NOW as GitHub issues (see filing instructions below).

4. **Does `tm projects deliverables`/`tm projects milestones` (§10.8) correctly supersede DOC-30's
   originally-reserved `tm projects plan ...` sub-namespace (§2.2 item 5), now that DOC-30 is
   retired?** This revision assumes yes (the content moved, so the reservation is moot) — confirm,
   or state a preference for the `plan` sub-namespace wording instead.

   **Decision (2026-07-10):** `tm projects plan` namespace: NOT reserved. Deliverable/milestone CLI
   stays nested under `tm projects` as drafted. DOC-30 is retired; no namespace reservations for
   retired visions.

5. **Storage placement:** §10.7 proposes `deliverables.json`/`milestones.json` as new daemon-owned
   sibling stores (central, cross-checkout). Confirm this over the alternative of nesting
   Deliverable/Milestone data inside a project's local `.trusty-mpm/config.toml` (§6's local-
   override precedence model) — the central placement was chosen because Deliverables/Milestones,
   like the Project record itself, need to be visible regardless of which checkout (if any) is
   currently open.

   **Decision (2026-07-10):** Deliverables/Milestones storage: CENTRAL (siblings to projects.json,
   keyed by repo_url), NOT local-checkout. Rationale: deliverables span many sessions and disposable
   worktrees; consistent with registry-B identity.

6. **Should `ticket_ref`/`spec_ref` (§10.2, §10.4) get the same opaque-placeholder treatment §6
   gave `jira_config`,** i.e. ship as a loosely-typed slot now with validation deferred, given
   #2082 (JIRA boards) is still open and may want to write through `ticket_ref` eventually?

   **Decision (2026-07-10):** ticket_ref: YES opaque-placeholder treatment (gh-first today, JIRA
   deferred, matching the repo's gh-only ticketing convention). spec_ref: NO abstraction — it is
   a plain repo-relative path into docs/specs/.

---

## 14. References

- **Epic #2108** — `tm project` deterministic CLI + multipane TUI control plane (this epic, the
  L3 substrate per §1.1).
- **Epic #2109** — `tm manager`, the L3 inference/portfolio layer built on top of this epic;
  boundary contract in §11.
- **Issue #1440** — TELUI-6, channel-agnostic SM proxy (inject/summarize across MCP/Telegram/
  Slack), the L2 surface per §1.1; PR pending.
- **Issue #1878** — DOC-30 retirement / salvage source for §10's Deliverable/Milestone carry-
  forward.
- **Child issues (filed 2026-07-06):** #2114 (registry-B HTTP surface), #2115 (`tm projects`
  CLI), #2116 (`tm sessions` rename/alias), #2117 (`/status` aggregation endpoint), #2118
  (multipane TUI skeleton, 3 panes + actions bar), #2119 (TUI live-refresh + activity wiring), #2120
  (deterministic configurator, CLI+TUI), #2121 (`gh_user` fail-loud validation), #2122
  (`jira_config` opaque placeholder), #2123 (deprecate registry A), #2124 (bare-`tm` entry-point
  transition, gated Phase 2).
- **Child issues (filed 2026-07-10, §12):** #2378 (Deliverable/Milestone CRUD API), #2379
  (`SessionRecord.deliverable_id`), #2380 (state-machine enforcement), #2381
  (`tm projects deliverables`/`milestones` CLI), #2382 (`/status` histogram extension), #2383
  (TUI Deliverable/Milestone view).
- **DOC-22** — Multi-Repo Session Routing: `docs/specs/multi-repo-session-routing.md` (registry
  B's origin, `Project`/`ProjectRegistry`/resolver/`fleet_by_project`).
- **DOC-26** — alpha-1 unified control plane: `docs/specs/trusty-mpm-alpha-1-control-plane.md`
  (session ID convention, `SessionActor`, activity observability pipeline this spec's Activity
  pane consumes).
- **DOC-16** — Interactive Sessions TUI: `docs/specs/sessions-tui-interactive.md` (poll cadence,
  ordinal addressing, active-session confirmation, `last_summary`/`summarizing` precedent this
  spec's Activity pane and keybindings follow).
- **DOC-30** — Project Manager vision: `docs/specs/DOC-30-project-manager-vision.md`. SUPERSEDED
  2026-07-10 by the #2108/#2109 split; the namespace collision originally flagged in §2 stands as
  historical record, but the Deliverable/Milestone layer DOC-30 proposed is now carried forward
  into §10 above rather than left unbuilt.
- **Issue #2081** (CLOSED) — `gh_user` on `Project`, `crates/trusty-mpm/src/project/record.rs`,
  `crates/trusty-mpm/src/core/gh_account.rs`.
- **Issue #2082** (OPEN) — JIRA boards to watch; referenced in §6/§9/§13 Q6 as a reserved, not yet
  designed, field.
- **Code:**
  - `crates/trusty-mpm/src/project/` — registry B (`record.rs`, `store.rs`, `registry.rs`,
    `resolver.rs`).
  - `crates/trusty-mpm/src/core/project.rs` — registry A (`ProjectInfo`).
  - `crates/trusty-mpm/src/bin/tm/commands/project.rs`, `cli.rs:1184-1201` — today's `tm project`
    (registry A).
  - `crates/trusty-mpm/src/bin/tm/commands/session.rs`, `cli.rs:1203-1600`+ — today's `tm
    session` (both families).
  - `crates/trusty-mpm/src/daemon/managed_routes/` (`fleet.rs`, `activity.rs`, `mod.rs`,
    `lifecycle.rs`) — the managed-session HTTP surface this spec reuses almost entirely.
  - `crates/trusty-mpm/src/mcp/tools/project.rs` — `project_register`/`project_get`/`project_list`
    MCP tools, mirrored to HTTP in §4.
  - `crates/trusty-mpm/src/tui/coordinator/`, `crates/trusty-mpm/src/tui/health/` — existing
    ratatui modules this spec's TUI (§5) follows structurally.
  - `crates/trusty-mpm/src/bin/tm/main.rs:230`, `commands/guided*.rs`, `commands/first_run.rs` —
    the bare-`tm` dispatch this spec's §7 proposes evolving.
  - `crates/trusty-mpm/src/session_manager/record.rs:28,136` — `ManagedSessionId`/`SessionRecord`,
    the struct §10.6 extends with `deliverable_id`.
  - `crates/trusty-mpm/src/session_manager/store.rs` — the atomic-write JSON store pattern §10.7's
    new `deliverables.json`/`milestones.json` stores follow verbatim.

---

## 15. Change log

- **2026-07-12 (v3.2, #2477)** — CLARIFIED pane wording for precision. The spec previously referred
  to a "4-pane" TUI layout, but the shipped implementation (#2118) is a 3-pane + actions-bar design:
  three boxed panes (Projects, Sessions, Activity) plus a 1-row contextual actions hint line. Updated
  §5, §7, §8 (slice 5), §9 (decision 3), §10.8, §12 (item #2383), §14 (references), and the 2026-07-06
  v2 changelog entry to consistently refer to "3 panes + actions bar" or "3 panes + contextual actions bar"
  to match the shipped behavior and avoid verification confusion.
- **2026-07-12 (v3.1, #2476)** — RESOLVED bug: decommissioned sessions were persisting in the
  Sessions pane (§5.1 item 2) instead of dropping on the next refresh tick, with the `(N)` header
  count never decrementing. Decided and implemented: filter tombstoned/decommissioned sessions OUT
  of the Sessions pane rather than keep them visible with a live-only count. §5.1 item 2 updated
  to document the resolved behavior.
- **2026-07-10 (v3)** — Owner articulated the three-layer communication model and retired DOC-30
  (issue #1878), directing its salvage into this spec. Added §1.1 (three-layer model, the new
  organizing frame for the whole document, mapping every existing/planned surface to L1/L2/L3);
  §4.1 (status-aggregation deterministic-only contract, design sketch for #2117); §10
  (Deliverable/Milestone data model, state machine, tier estimation, `spec_ref`, session↔
  deliverable binding, and the 12 DOC-30 rationales' carry-forward status, adapted to registry-B
  identity); §11 (explicit boundary contract with `tm manager` #2109 — what this epic will never
  do); §12 (proposed, unfiled work items WI-12–WI-17 for the Deliverable/Milestone layer); §13
  (six new open questions for the owner). §2–§9 unchanged; all section-number cross-references
  from the filed child issues (#2114–#2124) remain valid. Extended Spec ID range to
  `SPEC-PROJCTL-08~draft`.
- **2026-07-06 (v2)** — Owner resolved all six §9 open decisions from v1. Naming: sessions
  promoted to sibling top-level plural `tm sessions` (not nested under `tm projects`), `tm
  session` singular aliased+deprecated; registry A (`tm project init/list/info`) deprecated
  (not merged, not hard-cut); TUI ships 3 panes + actions bar v1 (no MVP cut); config UX ships as CLI+TUI
  in the same epic; bare-`tm` default flip gated on three explicit conditions (shipped+stable,
  guided/first-run preserved, dogfood period); `jira_boards` replaced with an opaque
  `jira_config` placeholder, concrete schema deferred to #2082. §2, §3, §5, §6, §7, §8, §9
  rewritten to match. Filed the 11-item child-issue breakdown under #2108 as #2114–#2124.
- **2026-07-06 (v1)** — Initial draft (DOC-35, `SPEC-PROJCTL-01~draft`). Design spec for #2108:
  `tm project` command tree, daemon API (mostly reuse of the already-shipped managed-session
  endpoints, plus a new HTTP surface for the MCP-only project registry), multipane TUI layout,
  deterministic configurator model, bare-`tm` entry-point transition plan, and an 11-item
  child-issue breakdown. Flagged a four-way "project" naming collision (registry A, registry B,
  DOC-30's unbuilt vision, and this epic) as the primary owner decision, with a recommended
  porcelain/plumbing reconciliation.
