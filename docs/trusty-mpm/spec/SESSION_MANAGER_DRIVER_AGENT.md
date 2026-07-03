# Session-Manager Driver Agent + Skill — Scoping Spec

> **Status:** Draft · 2026-06-15
> **Author:** Bob Matsuoka
> **Scope:** claude-mpm catalog content (agent definition + skill playbook)
> **Companion:** [SESSION_MANAGER_MVP.md](./SESSION_MANAGER_MVP.md)
> **Parent epic:** #380
>
> **Naming note (issue #1955):** example session names below (`tmpm-ticket-1234`,
> `tmpm-<slug>`) reflect the naming scheme in effect when this doc was written.
> Sessions are now named `tm-<project-leaf>-NN` (e.g. `tm-ticket-1234-01`); the
> daemon still recognizes the historical `tmpm-`/`trusty-mpm-` prefixes for
> already-running sessions. See `src/core/names.rs` for current behavior.

---

## 1. Purpose

The session-manager daemon (trusty-mpm) is a dumb harness launcher. It provisions
isolated workspaces, spawns Claude Code inside tmux sessions, exposes raw
observability, and waits. It does not reason. Reasoning is the calling agentic
process's job.

This document scopes the **driver agent** and **driver skill** — two pieces of
claude-mpm CATALOG content that together teach a calling agentic process (e.g.
Bob's CTO Claude MPM instance, a Unicorn Factory bot) how to operate the session
manager end-to-end.

### Key property: no additional LLM key required

When a calling agentic process drives the session manager, inference for interpreting
session activity is provided by claude-mpm's own Max-OAuth Claude — the same model
that powers the calling agentic process. The session manager's built-in OpenRouter
LLM classifier becomes OPTIONAL: a fallback for unattended/standalone operation only.
A `classification: null` response from the daemon is expected and acceptable; the
driver reads `raw_pane` directly and applies its own reasoning.

### Delivery mechanism

The driver agent and skill live in the **claude-mpm repository** under
`.claude/agents/` and `.claude/skills/`. They reach a trusty-mpm instance via the
existing catalog-sync (`tm catalog sync`) which fetches `repo/.claude/agents` and
`repo/.claude/skills` and caches them under `~/.trusty-mpm/catalog/`. The content
is then deployed into each managed session's isolated workspace by `prepare_session`.

---

## 2. The Driver Agent

### Role name

`harness-operator`

### What it is

A claude-mpm agent role definition (a markdown file living at
`.claude/agents/harness-operator.md` in the claude-mpm repo). When a calling agentic
process needs to spawn and supervise one or more trusty-mpm sessions, claude-mpm
delegates to the `harness-operator` agent role, which loads the driver skill and
takes over the interaction loop for that work.

### When claude-mpm delegates to it

The calling agentic process (e.g. the CTO Claude MPM instance) invokes
`harness-operator` when:

- A ticket or task requires spawning a new autonomous coding session in an isolated
  workspace.
- An existing session needs to be observed, answered, resumed, or decommissioned.
- A batch of tickets needs to be matched to sessions (unicorn-factory pattern).

### Authority and scope

| In scope | Out of scope |
|---|---|
| Calling the session-manager HTTP API | Making code correctness judgments independently |
| Interpreting raw pane output with Claude inference | Operating on the session manager daemon itself (starting/stopping the daemon process) |
| Applying the autonomy policy (auto-accept vs. escalate) | Modifying the isolated workspace contents directly |
| Injecting decisions via `POST /answer` | Inter-session coordination or routing |
| Surfacing escalations to the human via the calling agentic process | Anything requiring an Anthropic API key or OpenRouter key |
| Correlating a session to its PR/issue via GitHub API | |

The `harness-operator` agent does not embed its own LLM client. It uses the inference
provided by the calling agentic process (Max-OAuth Claude).

---

## 3. The Driver Skill

### Skill name

`session-manager-driver` (lives at `.claude/skills/session-manager-driver/SKILL.md`
in the claude-mpm repo)

The skill is a procedural playbook. It tells the calling agentic process exactly how
to operate the session manager, step by step.

### 3.1 Step 0 — Catalog sync

Before spawning any session the driver verifies that the local catalog cache is
current.

```
tm catalog sync            # fetch/update from claude-mpm repo (TTL: 24h default)
tm catalog sync --force    # bypass TTL and re-download
tm catalog ls              # verify at least one agent and one skill are cached
```

The catalog is stored under `~/.trusty-mpm/catalog/` and is deployed into each
new workspace by `prepare_session`. Sync is idempotent; if the cache is fresh the
command prints the cached counts and exits 0.

### 3.2 Step 1 — Spawn a session

```
tm sessions new \
  --repo  <https://github.com/org/repo> \
  --ref   <branch-or-sha> \
  --task  "<human-readable task description>" \
  [--name <hint-slug>]
```

Or via HTTP:

```
POST /api/v1/sessions/managed
{
  "repo_url": "https://github.com/org/repo",
  "ref": "main",
  "task": "Implement feature #1234 — add OAuth2 support",
  "name_hint": "ticket-1234"
}
```

Response (`201 Created`):

```json
{
  "id": "<uuid>",
  "name": "tmpm-ticket-1234",
  "workspace_path": "/Users/bob/.trusty-mpm/workspaces/repo/uuid",
  "repo_url": "...",
  "branch": "main",
  "state": "active",
  "created_at": "...",
  "attach_cmd": "tmux attach-session -t tmpm-ticket-1234"
}
```

The driver records the `id` for subsequent calls. The `workspace_path` and `branch`
fields are the correlation anchor for PR/issue lookup.

### 3.3 Step 2 — Poll activity and interpret the raw pane

```
GET /api/v1/sessions/managed/{id}/activity
```

Response always includes:

| Field | Type | Meaning |
|---|---|---|
| `raw_pane` | string | Last 60 lines of the tmux pane (always present) |
| `runtime_active` | bool | Whether the tmux session is alive |
| `state` | string | LLM classifier state or `"unknown"` if no key configured |
| `classification` | string or null | `null` when no OpenRouter key; string state when classifier ran |
| `pending_decision` | string or null | A decision question surfaced by the harness |
| `proposed_default` | string or null | The harness's suggested answer |

When `classification` is `null` (expected in the normal attended path), the driver
reads `raw_pane` directly and classifies the session state using its own Claude
inference. The driver applies the following classification heuristics against the
raw pane:

| Classification | Pane signals |
|---|---|
| `working` | Tool calls streaming, file edits, compilation output, test output |
| `idle` | REPL prompt visible, no recent output, cursor waiting |
| `blocked_on_permission` | "Allow", "Deny", "Trust" prompt visible; `[Y/n]` pattern |
| `errored` | Panic/backtrace, repeated error loop, `Error:` with no forward progress |
| `done` | Task completion marker, "Done", session summary output |

The driver must not treat the optional LLM `state` field as authoritative when
`classification` is `null`. The raw pane is the ground truth.

### 3.4 Step 3 — Inject input or answer pending decisions

**Send arbitrary text to the session pane:**

```
tm sessions send <id> "<text>"
```

HTTP: `POST /api/v1/sessions/managed/{id}/send  { "text": "..." }`

Use to ask the harness a question, provide guidance, or send a command without
attaching to the tmux pane.

**Answer a pending decision:**

When `pending_decision` is non-null, the driver applies the autonomy policy
(section 4) and either auto-accepts the proposed default or escalates.

To inject an accepted or overridden answer:

```
tm sessions answer <id> "<answer text>"
```

HTTP: `POST /api/v1/sessions/managed/{id}/answer  { "answer": "..." }`

This clears `pending_decision` and `proposed_default` on the session record and
injects the answer text into the tmux pane.

### 3.5 Step 4 — Manage session lifecycle

Sessions ENDURE until explicitly decommissioned. The running `claude` process is
transient inside an enduring session — stopping the runtime does not destroy the
workspace.

| Operation | CLI | HTTP | Effect |
|---|---|---|---|
| Stop runtime (keep workspace) | `tm sessions runtime-stop <id>` | `POST /api/v1/sessions/managed/{id}/runtime-stop` | Kills tmux session + claude process; workspace intact; state → `stopped`; resumable |
| Resume (restart runtime) | `tm sessions managed-resume <id>` | `POST /api/v1/sessions/managed/{id}/resume` | Re-spawns claude in the EXISTING workspace; no re-clone; state → `active` |
| Decommission (full teardown) | `tm sessions decommission <id>` | `POST /api/v1/sessions/managed/{id}/decommission` | Kills runtime, removes workspace from disk, state → `decommissioned`; tombstone record kept; no resume possible |
| Legacy stop alias | `tm sessions managed-stop <id>` | `DELETE /api/v1/sessions/managed/{id}` | Delegates to `runtime-stop`; backward-compatible |

**Lifecycle state machine:**

```
provisioning → active ↔ stopped
                  ↓
              errored (resumable after fix)
                  ↓
           decommissioned (terminal)
```

Daemon restart behavior: sessions with a live tmux session are re-adopted as
`active`; sessions whose tmux is gone but whose record survives are marked `stopped`
(resumable), not orphaned.

### 3.6 Step 5 — Observe progress beyond the pane

The driver can observe session progress through artifacts the harness produces,
not just terminal output. The substrate exposes the correlation anchor fields
`workspace_path`, `repo_url`, and `branch` on every session record. The driver uses
these to:

- Look up the corresponding GitHub PR (via `gh pr list --head <branch>`) and read
  its status, reviews, and CI checks.
- Look up the linked GitHub issue (by matching the branch name convention to the
  ticket number in the task description).
- Read commits the harness has pushed to confirm forward progress.
- Check trusty-review verdict on the diff to feed the autonomy policy auto-accept gate.

The session manager daemon does not perform these lookups. The driver is responsible
for correlating the session to its artifacts.

**Listing all sessions:**

```
tm sessions ls [--json]
```

HTTP: `GET /api/v1/sessions/managed`

Returns all sessions with their state, `pending_decision`, `workspace_path`,
`repo_url`, and `branch`.

**Getting one session record:**

```
GET /api/v1/sessions/managed/{id}
```

**Getting the tmux attach command (for human inspection):**

```
tm sessions attach <id>        # prints: tmux attach-session -t tmpm-<slug>
```

HTTP: `GET /api/v1/sessions/managed/{id}/attach-cmd`

---

## 4. Autonomy Operating Model

The calling agentic process targets ~80% auto-accept and ~20% human escalation.
This ratio is achievable because the trusty stack (trusty-search, trusty-memory,
trusty-review) provides structured guardrails that make automated decisions
trustworthy.

### Auto-accept gate (structured guardrails)

A proposed default MAY be auto-accepted only when ALL of the following signals are
favorable:

| Signal | Source | Check |
|---|---|---|
| Code review verdict | trusty-review (`review_diff` or `review_pr`) | APPROVE (no correctness findings) |
| CI / test status | GitHub PR checks or harness-run output | All required checks green |
| Search consistency | trusty-search (`search`) | No conflicting implementation found |
| Memory consistency | trusty-memory (`memory_recall`) | No blocking decision from prior context |

**SAFETY RULE (non-negotiable):** The auto-accept gate MUST NOT be driven by
reading the pane state alone. The `state` field and the optional `classification`
from the activity monitor are observability signals only. A session classified as
`working` does not imply its output is correct. Using a pane-reading classifier as
the approval gate would allow a subtly wrong harness to auto-merge bad code.

### Escalation triggers (~20%)

The driver escalates to the human when:

- trusty-review returns REJECT or produces correctness findings.
- CI is red with no obvious self-correctable error.
- The `pending_decision` text contains words indicating irreversible operations
  (e.g. "delete", "drop table", "push --force", "decommission").
- The session has been in `errored` state for more than a configurable threshold.
- The driver's own inference cannot confidently classify the pane state.
- The proposed default was rejected more than once in the same session.

### Tiered autonomy (T1–T4) — future direction

The unicorn-factory tiered PR autonomy model maps naturally onto this system:

| Tier | Decision type | Policy |
|---|---|---|
| T1 | Trivial / style-only change | Auto-accept without guardrail checks |
| T2 | Standard feature / bugfix with green CI + APPROVE | Auto-accept after all guardrail checks |
| T3 | Architecture-touching or cross-crate change | Auto-accept only with explicit trusty-review APPROVE + human-reviewed memory note |
| T4 | Irreversible or security-sensitive operation | Always escalate; human must accept |

The T1–T4 tier logic lives in the driver skill (the calling agentic process), not in
the session manager daemon. The daemon exposes the raw decision; the driver applies
the tier policy.

---

## 5. Interface Used

The driver skill calls the following concrete API surface. All HTTP routes are under
the daemon base URL discovered from `~/.trusty-mpm/daemon.lock` (default
`http://127.0.0.1:7880`).

### HTTP endpoints

| Method + Path | Purpose |
|---|---|
| `POST /api/v1/sessions/managed` | Spawn a new managed session |
| `GET /api/v1/sessions/managed` | List all sessions (state, pending_decision, workspace_path, repo_url, branch) |
| `GET /api/v1/sessions/managed/{id}` | Get one session record |
| `POST /api/v1/sessions/managed/{id}/send` | Inject arbitrary text into the pane |
| `GET /api/v1/sessions/managed/{id}/activity` | Get raw pane + structured state + pending_decision + proposed_default |
| `POST /api/v1/sessions/managed/{id}/answer` | Inject answer to pending decision; clears pending_decision |
| `GET /api/v1/sessions/managed/{id}/attach-cmd` | Return `tmux attach-session -t <name>` |
| `POST /api/v1/sessions/managed/{id}/runtime-stop` | Stop runtime, keep workspace (→ `stopped`) |
| `POST /api/v1/sessions/managed/{id}/resume` | Re-spawn runtime in existing workspace (→ `active`) |
| `POST /api/v1/sessions/managed/{id}/decommission` | Full teardown: kill runtime + remove workspace (→ `decommissioned`) |
| `DELETE /api/v1/sessions/managed/{id}` | Legacy alias for `runtime-stop` |

### CLI verbs (`tm` binary)

| Command | Maps to |
|---|---|
| `tm catalog sync [--force]` | `CatalogSync::sync` — fetches claude-mpm repo catalog to `~/.trusty-mpm/catalog/` |
| `tm catalog ls [--json]` | Lists cached agents and skills |
| `tm sessions new --repo --ref --task [--name]` | `POST /api/v1/sessions/managed` |
| `tm sessions ls [--json]` | `GET /api/v1/sessions/managed` |
| `tm sessions activity <id>` | `GET /api/v1/sessions/managed/{id}/activity` |
| `tm sessions send <id> <text>` | `POST /api/v1/sessions/managed/{id}/send` |
| `tm sessions answer <id> <answer>` | `POST /api/v1/sessions/managed/{id}/answer` |
| `tm sessions attach <id>` | `GET /api/v1/sessions/managed/{id}/attach-cmd` |
| `tm sessions runtime-stop <id>` | `POST /api/v1/sessions/managed/{id}/runtime-stop` |
| `tm sessions managed-stop <id>` | Alias for `runtime-stop` (backward-compatible) |
| `tm sessions managed-resume <id>` | `POST /api/v1/sessions/managed/{id}/resume` |
| `tm sessions decommission <id>` | `POST /api/v1/sessions/managed/{id}/decommission` |

---

## 6. Where It Lives and How It Is Delivered

### Authoring location (claude-mpm repo)

```
<claude-mpm-repo>/
└── .claude/
    ├── agents/
    │   └── harness-operator.md       # agent role definition
    └── skills/
        └── session-manager-driver/
            └── SKILL.md              # procedural playbook
```

These files follow the standard claude-mpm agent/skill conventions: the agent file
is a role definition with `role`, `purpose`, `authority`, and `when-to-invoke`
sections; the skill file is a `SKILL.md` playbook loaded on-demand.

### Delivery into trusty-mpm instances

The existing catalog-sync mechanism delivers the content:

1. `tm catalog sync` fetches `repo/.claude/agents` and `repo/.claude/skills` from
   the claude-mpm repository and caches them under `~/.trusty-mpm/catalog/`.
2. `prepare_session` (called by the workspace provisioner on `tm sessions new`) reads
   from `~/.trusty-mpm/catalog/` and deploys agents/skills into each new isolated
   workspace under `~/.trusty-mpm/workspaces/<project>/<session-id>/`.
3. The calling agentic process loads the `harness-operator` agent and
   `session-manager-driver` skill from its own user-level catalog
   (`~/.claude/agents/`, `~/.claude/skills/`) — the same location where `tm catalog
   sync` deposits them.

No changes to the trusty-mpm daemon are required to deliver or use the driver
content. The catalog-sync path is already live (merged in PR #1201).

---

## 7. Definition of Done / Acceptance

A working driver agent + skill must demonstrate all of the following in a live
end-to-end test driven by a calling agentic process (not a human typing CLI commands):

1. **Catalog sync**: the calling agentic process runs `tm catalog sync`, confirms at
   least one agent and one skill are cached, and loads the `harness-operator` agent
   role and `session-manager-driver` skill.

2. **Spawn**: the calling agentic process spawns a session with a real `repo_url`,
   `ref`, and `task` via `POST /api/v1/sessions/managed` and receives a valid session
   `id`, `workspace_path`, and `attach_cmd`.

3. **Observe**: the calling agentic process polls `GET .../activity` and correctly
   classifies the session state from `raw_pane` using its own Claude inference
   (without relying on `classification`, which may be `null`).

4. **Answer pending decision**: the calling agentic process detects a non-null
   `pending_decision`, applies the autonomy policy (auto-accept or escalate), and —
   if auto-accepting — calls `POST .../answer` with the chosen text; a subsequent
   activity poll shows `pending_decision: null`.

5. **Stop**: the calling agentic process calls `POST .../runtime-stop`; the session
   transitions to `stopped`; `runtime_active` is `false` on the next activity poll.

6. **Resume**: the calling agentic process calls `POST .../resume`; the session
   transitions to `active`; `runtime_active` is `true`; no re-clone occurred (the
   workspace timestamp predates the resume call).

7. **Decommission**: the calling agentic process calls `POST .../decommission`; the
   session transitions to `decommissioned`; the workspace directory no longer exists
   on disk; the tombstone record remains in `tm sessions ls` output.

8. **Autonomy policy**: during the cycle above, the driver auto-accepted at least one
   proposed default (using structured guardrail signals, not pane-reading alone) and
   escalated at least one decision to the human with a clear explanation of why it
   could not be auto-accepted.

---

## 8. Open Questions

| # | Question | Notes |
|---|---|---|
| OQ-1 | **How does the driver correlate a session to its PR/issue?** | The substrate exposes `repo_url`, `branch`, and `workspace_path`. The driver currently uses `gh pr list --head <branch>` to find the PR. Is branch-name-to-ticket matching sufficient, or does the spawn request need an explicit `issue_url` field added to `SpawnRequest`? |
| OQ-2 | **How do escalations surface to the human?** | Options: (a) post a GitHub issue comment on the linked issue; (b) send a Slack message via the CTO project's integration; (c) write a trusty-memory note and wait for the next human session. The skill spec must pick or parameterize a default channel. |
| OQ-3 | **Rate-limit contention across N concurrent sessions** | All sessions share one Claude Max account. The `raw_pane` may surface a rate-limit message. Should the driver detect this and back off autonomously, or is this an implicit `errored` state that always escalates? |
| OQ-4 | **Skill loading at startup vs. on-demand** | Claude Code loads skills at startup only. The `session-manager-driver` skill must be deployed to `~/.claude/skills/` (user level) before the calling agentic process session starts. Does `tm catalog sync` write to `~/.claude/skills/` directly, or does the user run a separate `tm install` step? The current `CatalogSync` writes to `~/.trusty-mpm/catalog/`; a separate install step into `~/.claude/skills/` may be needed. |
| OQ-5 | **T1–T4 tier criteria in the skill** | The tier criteria are sketched in section 4. The concrete guardrail checks (trusty-review call sequence, CI status polling interval, memory note format) need to be specified before the skill can be implemented. |
| OQ-6 | **Driver agent identity in provisioned workspaces** | When `harness-operator` spawns a session, the provisioned workspace receives agents/skills from the claude-mpm catalog. Should `harness-operator` itself be deployed INTO each managed session workspace (so the harness can recognize its controller), or is `harness-operator` exclusively a calling-agentic-process-side role? |
