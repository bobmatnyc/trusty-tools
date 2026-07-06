# DOC-35 — `tm project`: Deterministic Project/Session Control Plane (CLI + Multipane TUI)

**Status:** Draft
**Subsystem:** trusty-mpm — control plane / CLI / TUI / daemon API
**Owner:** Engineering (trusty-mpm)
**Last-updated:** 2026-07-06
**Spec ID:** `SPEC-PROJCTL-01~draft` … `SPEC-PROJCTL-06~draft` (DOC-35)
**Builds on:** DOC-22 — Multi-Repo Session Routing (`docs/specs/multi-repo-session-routing.md`);
DOC-26 — trusty-mpm alpha-1 unified project/session control plane
(`docs/specs/trusty-mpm-alpha-1-control-plane.md`); DOC-16 — Interactive Sessions TUI
(`docs/specs/sessions-tui-interactive.md`); DOC-30 — Project Manager: Vision & Lifecycle
Orchestrator (`docs/specs/DOC-30-project-manager-vision.md`).
**Cross-ref:** epic **#2108** (`tm project` — deterministic CLI + multipane TUI project/session
control plane, main entry point); issue **#2081** (project `gh_user`, CLOSED/shipped); issue
**#2082** (JIRA boards to watch, OPEN); epic **#1517** (multi-project awareness); epic **#1272**
(sessions TUI); the tmux-lifecycle "single owning `Session`, no fire-and-forget" standard (#1452).

> **Scope note.** This is a **design spec**: it proposes the `tm project` command tree, the
> daemon endpoints it consumes, the multipane TUI layout, the deterministic configurator model,
> and — because investigation surfaced a real naming collision — a **naming reconciliation**
> between this epic's `tm project` and three pre-existing "project" surfaces. It states *what*
> should be built and *why*, flags every owner-level fork in the road, and closes with a
> child-issue breakdown. **It carries no Rust changes.**

---

## 1. Overview and principles

### 1.1 What this is

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

### 1.2 Daemon as source of truth

Every verb in this spec — CLI or TUI — is a thin client over the `tm` daemon's HTTP API
(`crates/trusty-mpm/src/daemon/`). No client-side state is authoritative; the CLI/TUI read
`--json` snapshots and re-poll after mutations, mirroring the pattern DOC-16 already established
for the sessions TUI (`tui/coordinator/poll.rs`, timer + immediate re-poll-after-mutation).

### 1.3 "Main entry point once tm is stable"

Today, bare `tm` (no subcommand) dispatches to `commands::guided::run_guided_default`
(`crates/trusty-mpm/src/bin/tm/main.rs:230`, cli.rs doc-comment "the guided default fires
(#1708)") — an **in-project, cwd-scoped** spawn/reconnect flow. It is not a project browser and
has no multi-project view. §7 proposes how `tm project` supersedes this as the landing surface
without breaking the existing guided/first-run flows (`commands::first_run`, `commands::guided*`
in `crates/trusty-mpm/src/bin/tm/commands/`).

---

## 2. Naming reconciliation — OWNER DECISION REQUIRED

Investigation surfaced **three pre-existing surfaces** that already use the word "project" (one
of them literally the `tm project` CLI verb), plus the existing `tm session` verb family this
epic must coexist with or absorb. Presenting all four together is the point: any naming decision
here has to account for all of them at once, not just `tm project` vs `tm session`.

### 2.1 The four surfaces

| # | Surface | Backing type | Identity | CLI today | Status |
|---|---|---|---|---|---|
| A | **Directory registration** | `core::project::ProjectInfo` (`crates/trusty-mpm/src/core/project.rs`) — `{path, name, registered_at}` | absolute filesystem path | `tm project init/list/info` (`bin/tm/commands/project.rs`) → `POST/GET /projects`, `/projects/current`, `/projects/discover` | **Implemented, in use** |
| B | **NL-routing / session-spawn registry** | `project::Project` (`crates/trusty-mpm/src/project/record.rs`) — `{name, repo_url, default_branch, stack_hint, tags, description, gh_user}` | git `repo_url` | **MCP-only**: `project_register`/`project_get`/`project_list` (`mcp/tools/project.rs`); no CLI, no HTTP route | **Implemented (DOC-22, #1517), MCP-only** |
| C | **Project Manager vision** (DOC-30) | Unbuilt `Project`/`Deliverable`/`Milestone` model | git repo, 1:1 | Proposes `tm project create/show/add-deliverable/spawn-session/status` | **0% implemented — design only** |
| D | **This epic (#2108)** | TBD (§2.2 recommends reusing B) | — | Wants `tm project list/config/sessions/launch/kill/resume/decommission/status/attach` | **New** |

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

### 2.2 Recommended reconciliation

**Recommendation (flagged for owner sign-off):**

1. **`tm project` (this epic) adopts registry B as its backing store**, and gains the HTTP
   surface B has never had (today B is MCP-only — §4). Registry A's `init/list/info` behavior
   (directory registration + `.trusty-mpm/` scaffold, `scaffold_project_dir` in
   `bin/tm/commands/project.rs:105`) is **folded in** as the "register a local checkout for this
   project" operation, keyed by B's `name`/`repo_url` rather than a bare path. Concretely: `tm
   project config <name> --dir <path>` runs the same scaffold `init` already does, but writes
   into the entry the daemon already track under B, so a project registered once can have
   multiple local checkouts (worktrees) all resolving to the same config.
2. **`tm session` verbs are NOT renamed.** They become the **plumbing**; `tm project` is the
   **porcelain** — a git-style split. `tm project sessions <name>` and `tm project
   launch/kill/resume/decommission/attach` are thin wrappers that call the *same* daemon
   endpoints `tm session ls/new/decommission/...` already call (§4), scoped to one project. Old
   scripts, muscle memory, and docs referencing `tm session ...` keep working unchanged.
3. **DOC-30's future CLI namespace is the one that must yield.** DOC-30 is 0% implemented
   (`docs/specs/DOC-30-project-manager-vision.md:3`), so renaming its *proposed* verbs
   (`create`, `show`, `add-deliverable`, `add-milestone`, `spawn-session`, `status`) costs
   nothing today versus renaming a shipped surface. Recommend DOC-30, when picked up, nests
   under `tm project plan ...` (deliverables/milestones/estimation) rather than colliding with
   this epic's `tm project list/config/sessions/...`. This spec does not modify DOC-30; it only
   flags the collision so DOC-30's next revision reserves a sub-namespace.

**Tradeoffs considered and rejected:**

- *Full rename* — move `tm session <verb>` under `tm project session <verb>`, keep `tm session`
  as a **hidden deprecated alias** (there is already precedent for this pattern in
  `cli.rs:1433-1459`: `ManagedStop`/`RuntimeStop`/`ManagedResume` are `#[command(hide = true)]`
  aliases that print a deprecation notice). Cleanest long-term single surface, but real
  migration cost: every doc, skill (`tm-session-management`, `tm-session-pause`,
  `tm-session-resume` in `.claude/skills/`), and script referencing `tm session` needs updating
  or a grace-period alias. **Rejected for v1**; revisit once `tm project` has proven itself.
- *Merge A and B into one struct now* — technically cleaner (one `Project` type, one identity),
  but A's path-identity and B's `repo_url`-identity solve genuinely different problems (a bare
  local directory with no remote is a legitimate A-only case — `derive_name_from_url` returning
  `None` is exactly this, DOC-26 §14.4 "no remote → parent/dir slug"). Recommend **B absorbs A's
  behavior, not A's data model** — see item 1.

**OWNER DECISION:** confirm (a) B is the identity backbone for `tm project`, (b) `tm session`
stays as-is (porcelain/plumbing split, not renamed), (c) DOC-30 reserves `tm project plan` when
it is picked up.

---

## 3. CLI command tree

All verbs exist as `tm project <verb>` and, per §1.2, are thin HTTP clients. Every list/show verb
supports `--json` for scripting (matching the existing convention in `session.rs Ls { json: bool
}`, `cli.rs:1360-1364`).

```
tm project list [--json] [--tag <tag>]
    # GET /api/v1/projects  → table: name, repo_url, default_branch, gh_user, session counts by state, last_used_at

tm project register <name> --repo-url <url> [--default-branch <b>] [--description <s>]
                     [--tags <a,b,c>] [--stack-hint <s>] [--gh-user <login>]
    # POST /api/v1/projects  (idempotent upsert — mirrors project_register MCP tool, §4)

tm project config <name>
    # GET /api/v1/projects/{name}  → full config, human or --json
tm project config <name> set <field> <value>
tm project config <name> unset <field>
    # PATCH /api/v1/projects/{name}  — deterministic field=value forms, NOT free text.
    # Fields (v1): default_branch, description, tags (append/remove via --add/--remove),
    #              stack_hint, gh_user (#2081, shipped — validated against `gh auth status`).
    # Field (reserved, lands with #2082): jira_boards (board key/id + instance URL).
tm project config <name> --dir <path>
    # Local-checkout scaffold (today's `project init`, folded in per §2.2): creates
    # <path>/.trusty-mpm/{config.toml,sessions/} and links the checkout to the registry
    # entry `<name>` rather than a bare path.

tm project sessions <name> [--json] [--all]
    # GET /api/v1/sessions/managed/fleet, filtered to one project — reuses the EXISTING
    # fleet_by_project_route (crates/trusty-mpm/src/daemon/managed_routes/fleet.rs) which
    # already groups SessionSummary rows by Project. No new daemon logic needed for the
    # grouping itself; only a `?project=<name>` filter param (§4) needs adding.

tm project launch <name> --task "<text>" [--ref <branch>] [--name-hint <hint>]
                  [--runtime claude-code|tcode]
    # POST /api/v1/sessions/managed with repo_url/default_branch resolved FROM the
    # project registry — the operator never re-types the URL. Equivalent to today's
    # `tm session new <repo> --git-ref <r> --task <t>` but repo_url/ref are implied.

tm project kill <name> <session-id-or-ordinal> [--force]
    # POST /api/v1/sessions/managed/{id}/runtime-stop  (workspace preserved, resumable)
tm project resume <name> <session-id-or-ordinal>
    # POST /api/v1/sessions/managed/{id}/resume
tm project decommission <name> <session-id-or-ordinal>
    # POST /api/v1/sessions/managed/{id}/decommission  (terminal, tombstoned)
tm project attach <name> <session-id-or-ordinal>
    # GET /api/v1/sessions/managed/{id}/attach-cmd → prints the `tmux attach -t ...` command
    #   (does not itself shell out — mirrors `tm session attach` semantics if/when it exists;
    #   today the closest analog is the managed `attach-cmd` route).

tm project status <name> [--json]
    # Rollup: session counts by ManagedSessionState, last activity across sessions,
    # config completeness (gh_user set? jira configured?). New aggregation endpoint (§4).

tm projects
    # Alias for `tm project list` — matches epic wording ("tm projects" for the list view).
```

**Ordinal addressing.** `<session-id-or-ordinal>` accepts either the full `ManagedSessionId` or
the 1-based ordinal from the most recent `tm project sessions <name>` listing — mirroring DOC-16
§5.2's `/<n>` inline-addressing convention, so operators do not have to copy/paste UUIDs.

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
| `GET /api/v1/sessions/managed/fleet?project=<name>` | **EXTEND** | Existing `fleet_by_project_route` (`daemon/managed_routes/fleet.rs:61`) already groups by project; add an optional filter param — no new grouping logic. |
| `POST /api/v1/sessions/managed` | **REUSE** | `launch`, pre-filled `repo_url`/`default_branch` from the project record. |
| `POST /api/v1/sessions/managed/{id}/runtime-stop` | **REUSE** | `kill` (resumable stop). |
| `POST /api/v1/sessions/managed/{id}/resume` | **REUSE** | `resume`. |
| `POST /api/v1/sessions/managed/{id}/decommission` | **REUSE** | `decommission` (terminal). |
| `GET /api/v1/sessions/managed/{id}/attach-cmd` | **REUSE** | `attach`. |
| `GET /api/v1/sessions/managed/{id}/activity` | **REUSE** | Per-session status line (§5) — already returns `state`, `summary`, `pending_decision`, `raw_pane` (`daemon/managed_routes/activity.rs:44-90`). |
| `GET /api/v1/projects/{name}/status` | **NEW** | Aggregation: session counts by `ManagedSessionState`, most recent `last_activity_at`, config-completeness flags. Thin composition over existing `SessionManager::list()` + `ProjectRegistry::get()` — no new persistence. |

**Everything is deterministic and daemon-hosted**, consistent with #2108's core principle — no
endpoint here invokes an LLM classifier; the optional OpenRouter summarizer (DOC-16 D1,
`activity.rs`'s `classification` field) remains an opt-in overlay the TUI may display but never
depends on.

---

## 5. Multipane TUI

### 5.1 Pane layout (ASCII mockup)

```
┌─ tm project ──────────────────────────── v0.x.y · daemon ● http://127.0.0.1:7880 ┐
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
   .../fleet?project=<name>` (§4).
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

The deterministic edit surface for `tm project config <name> set/unset` (§3):

| Field | Type | Persisted in | Validation |
|---|---|---|---|
| `default_branch` | string | registry B, `~/.trusty-mpm/projects.json` | non-empty |
| `description` | string | registry B | none |
| `tags` | `Vec<String>` | registry B | `--add`/`--remove`, no free-text replace-whole-list footgun |
| `stack_hint` | string | registry B | none (advisory only) |
| `gh_user` | string | registry B (`Project::gh_user`, #2081, already shipped) | must appear in `gh auth status` account list — **fail loudly** per #2081's explicit requirement, never silently accept an unauthenticated login |
| `jira_boards` | `Vec<{board_id, instance_url}>` | registry B (**new field, lands with #2082** — currently unimplemented; this spec reserves the slot in the configurator's field table so `tm project config` does not need a second schema revision when #2082 ships) | board-id/URL syntax; auth via env/token per #2082's stated secret conventions |
| local checkout scaffold (`--dir`) | path | `.trusty-mpm/config.toml` in that checkout (unchanged scaffold from `scaffold_project_dir`) | path must be writable |

**Precedence (unchanged from the existing system, stated for clarity):** a local checkout's
`.trusty-mpm/config.toml` may layer further *local-only* overrides on top of the daemon-owned
registry B entry; the registry entry is the cross-checkout source of truth. This mirrors the
scope model already articulated in issue #920's RFC ("project config overrides system default")
and requires no new precedence machinery.

**Deterministic forms, not free text (explicit requirement):** every mutation is
`set <field> <value>` / `unset <field>` / `--add`/`--remove` for list fields — never a prompt
that accepts arbitrary prose. The TUI's config view (§5.2 `c` key) is a fixed-field form, not a
chat box.

---

## 7. "Main entry point" behavior

**Current state:** bare `tm` → `commands::guided::run_guided_default` (`bin/tm/main.rs:230`), a
cwd-scoped spawn/reconnect flow with no multi-project awareness. `commands::first_run` handles
true first-time setup.

**Proposed transition (two phases, both requiring owner sign-off — §9):**

1. **Phase 1 (opt-in).** Add a config flag (e.g. `ui.default_landing: "guided" | "project"` in
   `~/.trusty-mpm/config.toml`, `MpmConfig`, `core/config.rs`), default `"guided"` (no behavior
   change). Operators who want the new dashboard opt in explicitly.
2. **Phase 2 (default flip, gated on stability).** Once `tm project` has shipped and been used
   for a period TBD by the owner, flip the default: bare `tm` with **zero registered projects**
   still routes to `commands::first_run` (onboarding, unchanged — there is nothing to browse);
   bare `tm` with **≥1 registered project** lands on the `tm project` multipane dashboard (§5)
   instead of the guided cwd-scoped flow. The guided flow remains reachable explicitly (e.g.
   `tm guided` or `tm session start`) for the cwd-scoped launch/reconnect use case it already
   serves well — it is not deleted, only demoted from "default when no args" to "explicit verb."

This phasing avoids a hard cutover that breaks the muscle memory of existing `tm` (no args)
users while giving the owner an explicit decision point (not a silent default change) before
Phase 2 ships.

---

## 8. Child-issue breakdown

Numbered slices, each independently implementable and reviewable, to file as children of #2108:

1. **Registry-B HTTP surface** — `GET/POST /api/v1/projects`, `GET/PATCH
   /api/v1/projects/{name}` (§4), mirroring the existing MCP tools so the CLI/TUI have a non-MCP
   path. Gate: HTTP round-trip tests parallel to the existing MCP tool tests.
2. **`tm project` CLI: list/register/config** — §3's `list`, `register`, `config ... get/set/unset`
   verbs wired to slice 1. Gate: `cli_parses_project_*` tests + integration test against a live
   daemon.
3. **`tm project` CLI: sessions/launch/kill/resume/decommission/attach** — thin wrappers over
   the already-shipped managed-session endpoints (§4 REUSE rows), plus the `?project=` filter
   extension to `fleet_by_project_route`. Gate: integration test spawning via `tm project
   launch`, verifying it appears in `tm session ls --source-id`.
4. **`GET /api/v1/projects/{name}/status` aggregation endpoint** — session-count rollup +
   config-completeness flags (§4). Gate: unit test with a fixture registry + session set.
5. **Multipane TUI skeleton** — Projects/Sessions/Activity/Actions panes (§5.1), keyboard nav
   (§5.2), built as a new module under `crates/trusty-mpm/src/tui/` (e.g. `tui/project_ctl/`,
   split per the 500-SLOC convention: `layout`, `state`, `poll`, `panes/*`). Gate: manual + the
   existing TUI test patterns (`tui/coordinator/tests.rs`, `tui/health/tests.rs`) as a template.
6. **TUI live-refresh + activity wiring** — poll cadence, re-poll-after-mutation, activity-pane
   consumption of `GET .../{id}/activity` (§5.3, §5.4). Gate: TUI state-machine unit tests for
   refresh timing (mirroring DOC-16 STUI-8's pattern).
7. **Local-checkout scaffold fold-in** — `tm project config <name> --dir <path>` reusing
   `scaffold_project_dir`, keyed to a registry-B entry instead of a bare path (§2.2 item 1,
   §3). Gate: regression test that `tm project init`'s old behavior (still available via `tm
   project config --dir` or a compatibility alias — owner to confirm in §9) is unchanged for
   existing scripts.
8. **`gh_user` validation wiring in `PATCH /api/v1/projects/{name}`** — enforce the #2081
   fail-loud contract (`gh auth status` check) at config-write time, not just at `gh`-call time.
   Gate: unit test with a mocked `gh auth status` fixture.
9. **`jira_boards` field reservation** — add the (initially unused) schema slot to `Project` /
   `ProjectConfig` now, gated behind a no-op until #2082 lands, so the configurator's field
   table (§6) does not need a breaking schema bump later. Gate: serde round-trip test showing
   the field defaults to absent/empty and is forward-compatible.
10. **Bare-`tm` entry-point Phase 1 (opt-in flag)** — `ui.default_landing` config field (§7),
    wired but defaulting to unchanged behavior. Gate: config test + manual verification that
    `tm` with no flag set behaves identically to today.
11. **Bare-`tm` entry-point Phase 2 (default flip)** — **BLOCKED on owner sign-off** (§9); not to
    be started until the owner approves the criteria and timing from §7.

---

## 9. Open decisions for the owner

1. **Naming reconciliation (§2.2).** Confirm: (a) registry B (`project::Project`,
   `repo_url`-keyed) is the identity backbone for `tm project`, not registry A
   (`core::project::ProjectInfo`, path-keyed); (b) `tm session <verb>` stays as-is
   (porcelain/plumbing split — `tm project` wraps it, does not replace or rename it); (c) DOC-30,
   when picked up, reserves `tm project plan ...` rather than colliding with this epic's verbs.
2. **Registry A fold-in mechanics (§2.2, §8 slice 7).** Should the existing `tm project
   init/list/info` verbs (registry A) be kept as a compatibility alias indefinitely, deprecated
   with a notice (mirroring the `ManagedStop`/`RuntimeStop` hidden-alias pattern), or hard-cut
   once slice 7 ships? This spec does not pick one.
3. **TUI scope/MVP cut (§5).** Is the four-pane layout (Projects/Sessions/Activity/Actions) the
   v1 target, or should v1 ship Projects+Sessions only (two panes) with Activity/Actions as a
   fast-follow, to reduce the first PR's size? The child-issue breakdown (§8, slices 5–6) can be
   split further if the owner wants a smaller first cut.
4. **Deterministic-config UX (§6).** Is CLI-flag-based `set field value` / `unset field`
   sufficient for v1, or does the owner want an interactive-but-still-deterministic form in the
   TUI (fixed fields, tab-through, no free text) from day one? Both are described in §5.2/§6;
   the owner should confirm whether the TUI config view ships in the same PR as the CLI verb or
   later.
5. **Bare-`tm` entry-point timing (§7, §8 slice 11).** What criteria/duration gates the Phase-2
   default flip (e.g. "N weeks after Phase 1 ships with no regressions," "opt-in usage exceeds
   X," or simply "owner says go")? This spec intentionally leaves the trigger unspecified.
6. **`jira_boards` schema shape (§6, §8 slice 9).** #2082 is still open/undesigned in detail —
   should this epic's slice 9 guess at a shape now (board id/key + instance URL + auth-token env
   var name, per #2082's own text) or wait for #2082 to land its own spec section before adding
   the reserved field? Recommend guessing now (cheap, additive, reversible) but flagging for
   owner confirmation since #2082 is a separate open issue this epic does not own.

---

## 10. References

- **Epic #2108** — `tm project` deterministic CLI + multipane TUI control plane (this epic).
- **DOC-22** — Multi-Repo Session Routing: `docs/specs/multi-repo-session-routing.md` (registry
  B's origin, `Project`/`ProjectRegistry`/resolver/`fleet_by_project`).
- **DOC-26** — alpha-1 unified control plane: `docs/specs/trusty-mpm-alpha-1-control-plane.md`
  (session ID convention, `SessionActor`, activity observability pipeline this spec's Activity
  pane consumes).
- **DOC-16** — Interactive Sessions TUI: `docs/specs/sessions-tui-interactive.md` (poll cadence,
  ordinal addressing, active-session confirmation, `last_summary`/`summarizing` precedent this
  spec's Activity pane and keybindings follow).
- **DOC-30** — Project Manager vision: `docs/specs/DOC-30-project-manager-vision.md` (the
  namespace collision flagged in §2; deliverables/milestones layer this spec explicitly does not
  build).
- **Issue #2081** (CLOSED) — `gh_user` on `Project`, `crates/trusty-mpm/src/project/record.rs`,
  `crates/trusty-mpm/src/core/gh_account.rs`.
- **Issue #2082** (OPEN) — JIRA boards to watch; referenced in §6/§9 as a reserved, not yet
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

---

## 11. Change log

- **2026-07-06** — Initial draft (DOC-35, `SPEC-PROJCTL-01~draft`). Design spec for #2108: `tm
  project` command tree, daemon API (mostly reuse of the already-shipped managed-session
  endpoints, plus a new HTTP surface for the MCP-only project registry), multipane TUI layout,
  deterministic configurator model, bare-`tm` entry-point transition plan, and an 11-item
  child-issue breakdown. Flags a four-way "project" naming collision (registry A, registry B,
  DOC-30's unbuilt vision, and this epic) as the primary owner decision, with a recommended
  porcelain/plumbing reconciliation.
