# RFC: Configurable Issue State-Management (Labels / Transitions / Assignee) via YAML

**Status:** Accepted
**Date:** 2026-06-15
**Accepted:** 2026-06-15 (owner sign-off)
**Issues:** [#1246] (this RFC)
**Related:** [#1237] (`TicketSystem` trait), [#1244] (`tm ticket` `TicketSystem`/`CommandRunner` seam), [#1220] (`~/.trusty-tools/<crate>/config.yaml` convention)
**Cross-repo:** bob-duetto/unicorn-factory ADR-0004 (harness → trusty-mpm migration), bob-duetto/unicorn-factory#100 (first consumer — adopt the YAML model), bob-duetto/unicorn-factory#103 (migration PR), bob-duetto/unicorn-factory#97 (epic)
**Author:** Bob Matsuoka

---

## 1. Problem Statement

The **Unicorn Factory** (bob-duetto/unicorn-factory) drives autonomous coding
agents ("unicorns") whose *entire visible state* lives in GitHub artifacts —
issues, labels, assignees, comments, milestones, PRs. The product north star is
unambiguous:

> **Everything an agent does must be observable through GitHub artifacts alone**
> — without running the harness or reading its logs.

Today the factory's issue **state machine** is hardcoded in the harness's
`src/unicorn/github_client.py` (plus `executor.py`, `ticket_builder.py`,
`manifest.py`):

- the **label set** that represents each state,
- the **allowed label transitions** (`queued → approved → active-development →
  done`, with `paused`/`blocked` halt states and `failed` as a terminal),
- the **assignee / identity model** (issues assigned to human reviewers; the
  `bob-unicorn` bot identity used only for git commit attribution),
- the **label seeding** logic (create-missing with fixed colors/descriptions).

This hardcoding has three problems:

1. **Wrong layer.** A state machine is *configuration*, not harness code. Editing
   the model means editing Python and redeploying the harness.
2. **Not reusable.** Other consumers (and other repos/projects) cannot reuse the
   model without copying the Python.
3. **Migration blocker.** ADR-0004 moves the harness from `claude-mpm` to
   **trusty-mpm**. The ADR explicitly names this RFC's subsystem as a dependency:
   issue state management must move **out of the harness** into a YAML-configurable
   subsystem **owned by trusty-tools**, with the Unicorn Factory as its first
   consumer (bob-duetto/unicorn-factory#100 is blocked on it).

### 1.1 Goals

- **Externalize** the label set, the state-machine transitions, and the
  assignee/identity model into a **YAML config** owned by trusty-mpm.
- Expose the operations as **`tm issue …` CLI verbs** built on the existing
  `TicketSystem` / `CommandRunner` seam (#1237 / #1244), so the Python harness
  consumes them by **shelling out to `tm`** and the YAML is the portable shared
  contract.
- Ship a **default YAML that reproduces the Unicorn Factory model exactly**
  (behavior-preserving migration). The exact model — label families, state
  lifecycle, transitions, and assignee/identity rules — is **confirmed from
  factory source** via the schema appended to #1246 (§2.5).
- Preserve the **visibility north star**: state is always reconstructable from
  GitHub artifacts (labels + assignee + comments), never only from harness state.

### 1.2 Non-goals (locked owner decisions — stated, not open)

- **No separate library crate.** This subsystem lives in **trusty-mpm only**,
  alongside the #1237 `TicketSystem` trait under
  `crates/trusty-mpm/src/bin/tm/commands/`.
- **No MCP tool.** The surface is `tm` CLI verbs. The Python harness consumes by
  shelling out to `tm`; the YAML config is the portable shared contract. An MCP
  surface (mirroring #1221) is a possible *later* follow-up, explicitly out of
  scope here.
- **No new config format.** YAML, aligned with the #1220
  `~/.trusty-tools/<crate>/config.yaml` convention.

---

## 2. Current Architecture (Source Evidence)

### 2.1 The `TicketSystem` / `CommandRunner` seam (#1237 / merged in #1244)

The `tm ticket` command already established exactly the seam this subsystem must
reuse and extend. The relevant types live under
`crates/trusty-mpm/src/bin/tm/commands/ticket/`:

**`runner.rs`** — the mockable process seam:

```rust
/// Captured result of running an external command.
pub(crate) struct CommandOutput {
    pub(crate) success: bool,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

impl CommandOutput {
    /// `Ok(stdout.trim())` on success, else an `anyhow` error with stderr.
    pub(crate) fn ok_or_stderr(&self, program: &str) -> anyhow::Result<String> { … }
}

/// A seam for running external programs (`gh`, `git`).
pub(crate) trait CommandRunner {
    fn run(&self, program: &str, args: &[&str]) -> anyhow::Result<CommandOutput>;
}

/// Production runner that spawns real processes via `std::process::Command`.
pub(crate) struct RealCommandRunner;
```

**`system.rs`** — the backend trait, its gh-backed impl, the normalised issue
value type, and the test fake:

```rust
/// A normalised issue fetched from a ticketing backend.
pub(crate) struct Issue {
    pub(crate) number: u64,
    pub(crate) title: String,
    pub(crate) body: String,
    pub(crate) labels: Vec<String>,   // <-- the current label set on the issue
    pub(crate) open: bool,
}

/// A ticketing backend: validate an issue, comment on it, …
pub(crate) trait TicketSystem {
    fn name(&self) -> &'static str;
    fn validate(&self, issue_number: u64) -> anyhow::Result<Issue>;
    fn comment(&self, issue_number: u64, body: &str) -> anyhow::Result<()>;
}

/// GitHub-backed `TicketSystem` driving the `gh` CLI over a `CommandRunner`.
pub(crate) struct GhTicketSystem<R: CommandRunner> { runner: R }
```

`GhTicketSystem::validate` already runs
`gh issue view <n> --json number,title,body,state,labels` and parses it. The
`TicketSystemKind` clap `ValueEnum` (`Gh` default, `Jira`/`Linear` stubs) selects
the backend. Every `gh_*` test in `system.rs` drives a `FakeRunner` that returns
queued `CommandOutput`s and records `(program, args)` for assertion — the exact
test pattern this RFC adopts.

> **This RFC extends `TicketSystem` with the label/assignee operations**
> (seed-labels, transition) rather than introducing a parallel abstraction. The
> existing `Issue.labels` field is the read side of the current state; the new
> methods are the write side.

### 2.2 CLI dispatch pattern (`tm ticket`)

`crates/trusty-mpm/src/bin/tm/cli.rs` declares the verb as a `clap` subcommand
variant on the `Command` enum:

```rust
Ticket {
    issue: String,
    #[arg(default_value_t = …)]
    system: crate::commands::ticket::system::TicketSystemKind,
    // … notes, runtime …
},
```

`crates/trusty-mpm/src/bin/tm/main.rs` dispatches it:

```rust
Command::Ticket { issue, system, notes, .. } =>
    commands::ticket::ticket(&client, &url, issue, system, notes, runtime).await,
```

`crates/trusty-mpm/src/bin/tm/commands/mod.rs` registers `pub(crate) mod ticket;`.
The new `tm issue …` verbs follow this exact pattern: a new `Issue { … }`
subcommand (with its own `IssueCmd` sub-subcommand enum), a `commands::issue`
module, and a `main.rs` dispatch arm.

### 2.3 The embedded-default + seed-on-disk precedent (`tm services`)

`crates/trusty-mpm/src/services/manifest.rs` already demonstrates the precise
"ship a default YAML embedded in the binary, then seed it to disk if absent"
pattern this RFC needs:

```rust
const DEFAULT_MANIFEST_YAML: &str = include_str!("…default manifest…");
let m: ServicesManifest = serde_yaml::from_str(DEFAULT_MANIFEST_YAML)?;
```

and `crates/trusty-mpm/src/bin/tm/commands/services.rs` writes a default to disk
with `serde_yaml::to_string(&default)?` when the file is missing. The default
issue-state YAML follows the same approach (embedded via `include_str!`, seeded
on first use, overridable on disk).

### 2.4 YAML support already present

`serde_yaml = "0.9"` is already a **workspace dependency** (root `Cargo.toml`,
`[workspace.dependencies]`) and is already a dependency of **trusty-mpm**
(`crates/trusty-mpm/Cargo.toml`: `serde_yaml.workspace = true`). The crate's
`core/error.rs` even carries a `#[from] serde_yaml::Error` thiserror variant. No
new dependency is required for this RFC.

> **Decision (RESOLVED 2026-06-15): reuse `serde_yaml 0.9` now, migrate before
> production.** `serde_yaml 0.9` is **unmaintained** (the upstream crate is
> archived). This RFC does **not** change the dependency as part of this work —
> the issue-state subsystem reuses the already-present `serde_yaml` so it adds no
> new surface. However, a **follow-up issue WILL be filed** to migrate the whole
> workspace off `serde_yaml` to a maintained successor (`serde_yml` or
> `serde-yaml-ng`) **before this subsystem ships to production consumers** (i.e.
> before unicorn-factory #100 goes live). The PM is filing that tracking issue;
> the migration is a separate cross-workspace concern that this RFC depends on
> but does not perform. See §10 open-question resolution.

### 2.5 The Unicorn Factory model (CONFIRMED from factory source via #1246)

> **Decision (CONFIRMED 2026-06-15): the model is now extracted directly from the
> factory source.** The owner appended the exact, source-extracted state-model
> schema to issue #1246 ("Concrete schema — current Unicorn Factory model"),
> derived from `src/unicorn/github_client.py`, `src/unicorn/executor.py`,
> `src/unicorn/ticket_builder.py`, and `src/unicorn/manifest.py` (with per-rule
> source-file line references reproduced in §4.3). This **supersedes** the earlier
> "assumed model" framing: the label set, colors, descriptions, lifecycle states,
> transition edges, and assignee/identity rules below are no longer placeholders —
> they are the authoritative values the default YAML reproduces verbatim. The
> implementation is no longer blocked on inaccessible source; behavior-preserving
> adoption (unicorn-factory #100) remains an acceptance criterion (§8, §9) but is
> now a **mechanical equality check against a known-good schema**, not a discovery
> exercise.

The confirmed model (full values in §4.2/§4.3, exactly as in #1246):

- **States (the `unicorn:*` lifecycle labels):** `queued` (initial, set at issue
  creation) → `approved` (human gate) → `active-development` (executor running) →
  `done` (terminal success). Two halt states branch from `active-development`:
  `paused` and `blocked` (human-applied; the watcher cancels the session in
  place). `failed` is the terminal failure state. **There is no `in-review`
  state** — the prior draft was wrong on this point. `done` and `failed` are the
  only terminal states.
- **Labels:** one GitHub label per state, all `unicorn:`-prefixed
  (`unicorn:queued`, `unicorn:approved`, `unicorn:active-development`,
  `unicorn:paused`, `unicorn:blocked`, `unicorn:done`, `unicorn:failed`), plus
  four non-state label families seeded by bootstrap: the ownership label
  `unicorn` (`7B68EE`), blast-radius labels (`blast:low/medium/high`), PR-tier
  labels (`T2`/`T3`/`T4`), and approval-level labels
  (`approval:level-1/2/3`). Exact colors/descriptions in §4.2.
- **Assignee/identity:** the strategy is **`bot_identity`**, but the bot identity
  (e.g. `bob-unicorn`, derived `{accountable_user_or_author}-unicorn`, pattern
  `^[a-z0-9][a-z0-9-]*-unicorn$`) is used **only for git commit attribution**
  (worktree-local `user.name`/`user.email` plus `Unicorn:` / `Unicorn-Issue:`
  commit trailers) — **not** as a GitHub assignee. Issues are assigned at creation
  to `manifest.github.review_assignees` (human reviewers); the harness does
  **not** reassign during transitions. Execution eligibility additionally requires
  the issue be assigned to `manifest.github.accountable_user` (a precondition
  check, not an assignment action). PR-tier and approval-level labels are applied
  to the **PR**, not the issue.
- **Seeding:** create-missing labels with fixed color/description, idempotently.

---

## 3. Target Architecture

### 3.1 Layering

```
  PYTHON HARNESS (unicorn-factory)        OPERATOR / CI
        |                                       |
        | shell out: `tm issue transition …`    | `tm issue seed-labels …`
        v                                       v
  +-------------------------------------------------------------+
  |  tm issue  (CLI verbs)   crates/trusty-mpm/src/bin/tm/      |
  |     commands/issue/                                          |
  |       mod.rs        dispatch + orchestration                |
  |       config.rs     YAML schema + load/discovery/validate   |
  |       state.rs      StateMachine: states + transition graph |
  |       ops.rs        seed_labels(), transition() operations  |
  +-----------------------------|-------------------------------+
                                |  reuses
                                v
  +-------------------------------------------------------------+
  |  TicketSystem / CommandRunner seam (ticket/system.rs,       |
  |  ticket/runner.rs)  — EXTENDED with label/assignee methods  |
  +-----------------------------|-------------------------------+
                                |  RealCommandRunner → `gh`
                                v
                          GitHub (labels, assignees, comments)
                                ^
                                |  the ONLY source of truth
  +-------------------------------------------------------------+
  |  YAML state model (the portable contract)                   |
  |   default: crates/trusty-mpm/examples/issue-state/          |
  |            unicorn-factory.yaml                              |
  |   on-disk: ./issue-state.yaml  OR                            |
  |            ~/.trusty-tools/trusty-mpm/issue-state.yaml       |
  +-------------------------------------------------------------+
```

The YAML is the shared contract. The harness does not parse it — it shells out to
`tm issue`, and `tm issue` is the single interpreter of the model. State remains
fully reconstructable from GitHub artifacts because every operation maps to a
concrete label/assignee mutation visible on the issue.

### 3.2 How `TicketSystem` is extended

The trait gains label and assignee operations. The names below are illustrative;
the implementation PR may refine signatures:

```rust
pub(crate) trait TicketSystem {
    fn name(&self) -> &'static str;
    fn validate(&self, issue_number: u64) -> anyhow::Result<Issue>;
    fn comment(&self, issue_number: u64, body: &str) -> anyhow::Result<()>;

    // --- NEW for #1246 ---

    /// List labels that already exist in the repo (name + color + description),
    /// so seeding can be idempotent (create-missing only).
    fn list_repo_labels(&self) -> anyhow::Result<Vec<RepoLabel>>;

    /// Create a label in the repo (idempotent at the call site via list diff).
    fn create_label(&self, label: &RepoLabel) -> anyhow::Result<()>;

    /// Add/remove a label on an issue (the atomic transition primitives).
    fn add_label(&self, issue: u64, label: &str) -> anyhow::Result<()>;
    fn remove_label(&self, issue: u64, label: &str) -> anyhow::Result<()>;

    /// Apply the assignee rule (assign a login, or clear all assignees).
    fn set_assignee(&self, issue: u64, who: AssigneeTarget) -> anyhow::Result<()>;
}
```

`GhTicketSystem` implements these over `gh`:

| Method            | `gh` invocation (illustrative)                                   |
|-------------------|------------------------------------------------------------------|
| `list_repo_labels`| `gh label list --json name,color,description`                    |
| `create_label`    | `gh label create <name> --color <hex> --description <desc>`       |
| `add_label`       | `gh issue edit <n> --add-label <name>`                           |
| `remove_label`    | `gh issue edit <n> --remove-label <name>`                       |
| (label swap)      | `gh issue edit <n> --add-label <new> --remove-label <old>` (single-call default, §5.2) |
| `set_assignee`    | `gh issue edit <n> --add-assignee <login>`; clear: `--remove-assignee <login>` per current assignee (§5.4) |

Every one of these is unit-tested behind `FakeRunner` (§7) — no live `gh`.

### 3.3 Breaking-change mitigation: default trait-method bodies for the stub backends

Extending `TicketSystem` with the six new methods (`list_repo_labels`,
`create_label`, `add_label`, `remove_label`, `set_assignee`, plus the read side
exposed by the existing `validate`/`Issue.labels`) is a **breaking change to the
trait**: the `Jira` and `Linear` stubs (`TicketSystemKind::Jira` / `Linear`)
would otherwise fail to compile until they implement all six.

**Chosen mitigation (Option 1 — default trait-method bodies that error).** Each
new method ships with a **default body that returns an error** so existing stubs
keep compiling unchanged:

```rust
pub(crate) trait TicketSystem {
    fn name(&self) -> &'static str;
    fn validate(&self, issue_number: u64) -> anyhow::Result<Issue>;
    fn comment(&self, issue_number: u64, body: &str) -> anyhow::Result<()>;

    // --- NEW for #1246: default bodies keep Jira/Linear stubs compiling ---

    fn list_repo_labels(&self) -> anyhow::Result<Vec<RepoLabel>> {
        anyhow::bail!("list_repo_labels not supported for this ticket system")
    }
    fn create_label(&self, _label: &RepoLabel) -> anyhow::Result<()> {
        anyhow::bail!("create_label not supported for this ticket system")
    }
    fn add_label(&self, _issue: u64, _label: &str) -> anyhow::Result<()> {
        anyhow::bail!("add_label not supported for this ticket system")
    }
    fn remove_label(&self, _issue: u64, _label: &str) -> anyhow::Result<()> {
        anyhow::bail!("remove_label not supported for this ticket system")
    }
    fn set_assignee(&self, _issue: u64, _who: AssigneeTarget) -> anyhow::Result<()> {
        anyhow::bail!("set_assignee not supported for this ticket system")
    }
}
```

`GhTicketSystem` **overrides all six** with real `gh`-backed implementations
(§3.2 table); the `Jira`/`Linear` stubs inherit the erroring defaults and so keep
compiling without modification. This is the **lowest-friction** option and is
consistent with the **no-new-abstraction** non-goal (§1.2): we extend the
existing trait rather than introducing a parallel label/assignee abstraction.
Each default returns a clear, user-facing
`"<op> not supported for this ticket system"` error, so a future `tm issue` run
against a non-`gh` backend fails loudly and legibly rather than silently. The
alternatives — a separate `LabelOps` trait, or making the methods non-default and
forcing every stub to implement them now — were rejected as higher-friction for
no current benefit (only `gh` is implemented today).

---

## 4. YAML Schema

> The schema below is the **exact, source-confirmed Unicorn Factory model** as
> appended to issue #1246 (§2.5). Label names, color hexes, descriptions,
> transition edges + triggers, and the assignee/identity model are transcribed
> verbatim from that schema.

### 4.1 Schema overview

| Key                          | Type         | Required | Meaning |
|------------------------------|--------------|----------|---------|
| `version`                    | integer      | yes      | Schema version (start at `1`). |
| `label_config.base`          | string       | yes      | Ownership/base label applied to every work item (`unicorn`). |
| `label_config.approved`      | string       | yes      | The approval gate label (`unicorn:approved`). |
| `label_config.blast_prefix`  | string       | yes      | Prefix for blast-radius labels (`blast:`). |
| `label_config.status_prefix` | string       | yes      | Prefix for the `unicorn:*` lifecycle labels (`unicorn:`). |
| `states[]`                   | list         | yes      | Ordered list of lifecycle states. |
| `states[].name`              | string       | yes      | Machine state name (unique key; used by `tm issue transition`). |
| `states[].order`             | integer      | no       | Display/sort ordering (informational; does not gate transitions). |
| `states[].terminal`          | bool         | no       | `true` for terminal states (`done`, `failed`); no outbound edges. |
| `states[].label.name`        | string       | yes      | GitHub label representing this state (the visible artifact). |
| `states[].label.color`       | string (hex) | yes      | 6-hex-digit color, no `#`. Used by `seed-labels`. |
| `states[].label.description` | string       | no       | Label description used by `seed-labels`. |
| `extra_labels[]`             | list         | yes      | Non-state label families (ownership/blast/PR-tier/approval) seeded by bootstrap. |
| `extra_labels[].name`        | string       | yes      | Label name (e.g. `blast:high`, `T2`, `approval:level-1`). |
| `extra_labels[].color`       | string (hex) | yes      | 6-hex-digit color, no `#`. |
| `extra_labels[].description` | string       | no       | Label description used by `seed-labels`. |
| `transitions[]`              | list         | yes      | Allowed `from → to` edges (with a trigger annotation). |
| `transitions[].from`         | string\|null | yes      | Source state name (or `null` for the creation edge `null → queued`). |
| `transitions[].to`           | string       | yes      | Destination state name (must match a `states[].name`). |
| `transitions[].trigger`      | enum         | yes      | What drives the edge: `issue_created` \| `human_label` \| `executor_start` \| `executor_complete` \| `executor_failure`. |
| `assignee_model.strategy`    | enum         | yes      | Assignment strategy. For the factory: `bot_identity`. |
| `assignee_model.identity_pattern` | string  | yes      | How the bot identity is derived (`{accountable_user_or_author}-unicorn`). |
| `assignee_model.git_attribution` | map      | yes      | Git `user.name`/`user.email` + commit trailers for attribution (worktree-local). |
| `assignee_model.per_state`   | map          | yes      | Per-state assignee rule (`{manifest.github.review_assignees}` at `queued`, else `unchanged`). |

**Assignee model semantics (factory `bot_identity` strategy):** the bot identity
(e.g. `bob-unicorn`) is used **only for git commit attribution** — worktree-local
`user.name`/`user.email` and the `Unicorn:` / `Unicorn-Issue:` commit trailers.
It is **never** set as a GitHub assignee. Issue assignees are set **once, at
creation**, to the human reviewers in `manifest.github.review_assignees`; the
harness does not reassign during transitions (every non-initial state is
`unchanged`). Execution eligibility additionally requires the issue be assigned to
`manifest.github.accountable_user` — a precondition check, not an assignment
action. PR-tier (`T2`/`T3`/`T4`) and approval-level (`approval:level-N`) labels
are applied to the **PR**, not the issue.

**Validation rules (enforced at load, before any `gh` call):**

1. `version` is recognised (`1`).
2. Every `states[].name` is unique and non-empty.
3. Every `transitions[].from` (when non-`null`) and `.to` references an existing
   state name; the single `null → queued` creation edge is the only `null` source.
4. Every `states[].label.color` and `extra_labels[].color` is a 6-hex-digit string.
5. `assignee_model.strategy` is recognised; for `bot_identity` the
   `identity_pattern` is present.
6. The transition graph is well-formed (no edge references a missing state); the
   terminal states (`done`, `failed`) have no outbound edges.

### 4.2 Complete annotated example (default = Unicorn Factory model)

> Committed at `crates/trusty-mpm/examples/issue-state/unicorn-factory.yaml` and
> embedded in the binary via `include_str!` as the default. **All label strings,
> colors, descriptions, transition edges, and assignee rules below are the exact,
> source-confirmed values from issue #1246 (§2.5/§4.3).**

**Entry state: `queued` is the initial state, set at issue creation.** The state
machine's initial state is **`queued`**, applied by `ticket_builder.py` at issue
creation (the `null → queued` edge, trigger `issue_created`). A human reviewer then
applies `unicorn:approved` to gate execution (`queued → approved`, trigger
`human_label`); the scheduler polls for that label. The executor then drives
`approved → active-development` (trigger `executor_start`) and on completion
`active-development → done` (trigger `executor_complete`) or on any exception
`active-development → failed` (trigger `executor_failure`). Two human-applied halt
edges branch from `active-development`: `→ paused` and `→ blocked` (trigger
`human_label`; the watcher cancels the session in place, leaving the halt label).
`done` and `failed` are terminal. (This corrects the earlier draft, which wrongly
treated `approved` as the externally-set initial state and included a non-existent
`in-review` state.)

```yaml
# state-management.yaml — default Unicorn Factory model
# Extracted from src/unicorn/github_client.py + executor.py
# All label names and colors are hardcoded in _canonical_labels() and
# applied by the executor; this YAML is a faithful, behavior-preserving
# externalization of that logic.

version: 1

# ---------------------------------------------------------------------------
# Label families
# Labels are organized into four families. All family prefixes are
# configurable; defaults produce the canonical unicorn:* / blast:* namespace.
# ---------------------------------------------------------------------------

label_config:
  base: "unicorn"            # workflow.labels.base  (manifest.py WorkflowLabelsConfig)
  approved: "unicorn:approved"  # workflow.labels.approved
  blast_prefix: "blast:"    # workflow.labels.blast_prefix
  status_prefix: "unicorn:" # workflow.labels.status_prefix (UNI-REQ-018)

# ---------------------------------------------------------------------------
# States (the unicorn:* lifecycle labels)
# ---------------------------------------------------------------------------

states:

  # Initial state — set at issue creation by ticket_builder.py
  - name: queued
    label:
      name: "unicorn:queued"
      color: "BFD4F2"
      description: "Queued: awaiting human review"
    order: 1

  # Gate state — human applies this label to approve execution
  - name: approved
    label:
      name: "unicorn:approved"
      color: "0E8A16"
      description: "Human-approved: execution eligible"
    order: 2

  # Running state — executor applies this label when the session starts
  - name: active-development
    label:
      name: "unicorn:active-development"
      color: "1D76DB"
      description: "Executor actively working"
    order: 3

  # Control-channel halt states — human applies; watcher polls for these
  - name: paused
    label:
      name: "unicorn:paused"
      color: "E4E669"
      description: "Execution intentionally suspended"
    order: 4

  - name: blocked
    label:
      name: "unicorn:blocked"
      color: "E11D48"
      description: "Execution halted; needs human input"
    order: 5

  # Terminal states
  - name: done
    label:
      name: "unicorn:done"
      color: "0075CA"
      description: "Execution complete; PR opened — terminal"
    terminal: true
    order: 6

  - name: failed
    label:
      name: "unicorn:failed"
      color: "B60205"
      description: "Execution failed — terminal"
    terminal: true
    order: 7

# ---------------------------------------------------------------------------
# Additional label families (not part of state machine, but seeded by bootstrap)
# ---------------------------------------------------------------------------

extra_labels:

  # Base / ownership label — applied to all issues at creation
  - name: "unicorn"
    color: "7B68EE"
    description: "Unicorn Factory work item"

  # Blast-radius labels — applied at issue creation from AWP blast_radius field
  - name: "blast:low"
    color: "BFD4F2"
    description: "Low blast radius"
  - name: "blast:medium"
    color: "FBCA04"
    description: "Medium blast radius"
  - name: "blast:high"
    color: "E11D48"
    description: "High blast radius"

  # PR-tier labels — applied to PRs by executor._create_tiered_pr()
  # blast:low → T4, blast:medium → T3, blast:high or missing → T2 (draft)
  - name: "T2"
    color: "E11D48"
    description: "Tier 2 PR: high blast — SELT/CTO review required"
  - name: "T3"
    color: "FBCA04"
    description: "Tier 3 PR: medium blast — 1 reviewer required"
  - name: "T4"
    color: "0075CA"
    description: "Tier 4 PR: low blast — CI passing is sufficient"

  # Approval-level labels — applied to PRs by executor._classify_and_gate()
  # Level matrix (blast × project_class):
  #   blast:low   × internal   → level 1 (auto-merge eligible)
  #   blast:low   × production → level 2
  #   blast:medium × any       → level 2
  #   blast:high   × any       → level 3
  - name: "approval:level-1"
    color: "0E8A16"
    description: "Approval level 1: trusty-review + CI sufficient"
  - name: "approval:level-2"
    color: "FBCA04"
    description: "Approval level 2: harness owner review required"
  - name: "approval:level-3"
    color: "E11D48"
    description: "Approval level 3: harness owner review required"

# ---------------------------------------------------------------------------
# State machine transitions
# ---------------------------------------------------------------------------

transitions:

  # Ticket creation: no prior state → queued (ticket_builder.py build_issue_payloads)
  - from: null
    to: queued
    trigger: issue_created
    description: "ticket_builder applies unicorn:queued at issue creation"

  # Human approval gate (human manually applies unicorn:approved label)
  # The scheduler (OnApproveScheduler) polls for this label to trigger execution.
  - from: queued
    to: approved
    trigger: human_label
    description: "Human reviewer applies unicorn:approved to gate execution"

  # Execution start: executor Phase 3 (add active-dev first, then remove approved)
  - from: approved
    to: active-development
    trigger: executor_start
    description: "executor.execute() Phase 3 — add new label first, then remove old"

  # Control-channel halt: human applies paused/blocked during active session
  # The _run_with_watcher polls get_issue() every poll_interval_seconds.
  # On detection: session is cancelled (subprocess killed); human label is left in place.
  - from: active-development
    to: paused
    trigger: human_label
    description: "Human applies unicorn:paused; watcher cancels session"

  - from: active-development
    to: blocked
    trigger: human_label
    description: "Human applies unicorn:blocked; watcher cancels session"

  # Happy path terminal: PR opened and (if level-1) auto-merged
  - from: active-development
    to: done
    trigger: executor_complete
    description: "executor Phase 6 success path — add done, remove active-development"

  # Failure terminal: any exception in execute() after worktree creation
  - from: active-development
    to: failed
    trigger: executor_failure
    description: "executor Phase 6 failure path — add failed, remove active-development"

# ---------------------------------------------------------------------------
# Assignee / identity model
# ---------------------------------------------------------------------------

assignee_model:

  # Strategy: the harness self-assigns issues using a named bot identity.
  # The identity is derived from the manifest at runtime (never hardcoded):
  #   - manifest.identity (explicit, e.g. "bob-unicorn") if set, else
  #   - "{manifest.github.accountable_user}-unicorn" if accountable_user is set, else
  #   - "{manifest.author}-unicorn"
  # Pattern enforced: ^[a-z0-9][a-z0-9-]*-unicorn$  (e.g. "bob-unicorn")
  # Source: manifest.py derive_identity() + _IDENTITY_PATTERN
  strategy: bot_identity
  identity_pattern: "{accountable_user_or_author}-unicorn"
  identity_example: "bob-unicorn"

  # Git attribution (commit trailers injected into every harness commit):
  #   Unicorn: {identity}
  #   Unicorn-Issue: #{issue_number}
  # Git user.name and user.email are set per-worktree (not globally):
  #   user.name  = {identity}
  #   user.email = {identity}@users.noreply.github.com
  git_attribution:
    user_name: "{identity}"
    user_email: "{identity}@users.noreply.github.com"
    commit_trailers:
      - "Unicorn: {identity}"
      - "Unicorn-Issue: #{issue_number}"
    scope: worktree_local  # git config --worktree; never touches shared .git/config

  # Assignment timing: issues receive assignees at creation from
  # manifest.github.review_assignees (the human reviewers, not the bot).
  # The coding-trigger eligibility check (UNI-REQ-014) additionally requires
  # the issue to be assigned to manifest.github.accountable_user before execution.
  per_state:
    queued:
      assignees: "{manifest.github.review_assignees}"  # human reviewers
      description: "Set at creation by ticket_builder; review_assignees for human approval gate"
    approved:
      assignees: unchanged
      description: "Human applies label; assignees unchanged from queued state"
    active-development:
      assignees: unchanged
      description: "Executor does not modify assignees; accountable_user must already be assigned"
    done:
      assignees: unchanged
    failed:
      assignees: unchanged
    paused:
      assignees: unchanged
    blocked:
      assignees: unchanged
```

### 4.3 Transition and assignee rules (prose summary, with source-file references)

**Transitions.** The state machine is linear for the happy path:
`null → queued → approved → active-development → done`. All transitions except
`queued → approved` (human-driven) are executor-driven. Two halt paths branch from
`active-development`: to `paused` or `blocked` (human-applied, session cancelled in
place), and to `failed` (any exception after the worktree is created). Terminal
states `done` and `failed` have no outbound transitions. All label transitions are
atomic in the sense of "add new first, then remove old" so the issue is never in a
zero-label state.

**Assignee model.** Issues are created with `manifest.github.review_assignees` as
assignees (human reviewers, not the bot). The harness itself never explicitly
reassigns during state transitions. The coding-trigger eligibility check
(`executor._check_eligibility`) gates execution on the issue being assigned to
`manifest.github.accountable_user` — this is a precondition check, not an
assignment action. The harness identity (e.g. `bob-unicorn`) is used only for
**git commit attribution** (worktree-local `user.name`/`user.email` + commit
trailers `Unicorn:` / `Unicorn-Issue:`), not as a GitHub assignee. PR-level labels
(`T2`/`T3`/`T4`, `approval:level-N`) are applied to the **PR**, not the issue, by
the executor after the PR is opened.

**Source-file references (from #1246):**

- `src/unicorn/github_client.py` lines 70–133: `_canonical_labels()` — all label
  names, colors, descriptions
- `src/unicorn/github_client.py` lines 373–418: `add_labels()` / `remove_label()`
  — atomic transition primitives
- `src/unicorn/executor.py` lines 695–710: Phase 3 (approved → active-development)
- `src/unicorn/executor.py` lines 790–800: Phase 6 success (active-development → done)
- `src/unicorn/executor.py` lines 820–835: Phase 6 failure (active-development → failed)
- `src/unicorn/executor.py` lines 946–949: control-channel halt labels (paused, blocked)
- `src/unicorn/executor.py` lines 1535–1558: worktree-local git identity setup (UNI-REQ-017)
- `src/unicorn/ticket_builder.py` lines 240–255: issue creation labels (base + blast: + :queued)
- `src/unicorn/manifest.py` lines 233–251: `WorkflowLabelsConfig` — configurable label prefixes
- `src/unicorn/manifest.py` lines 613–660: `derive_identity()` — bot identity derivation

---

## 5. Operations

Each operation is a `tm issue` verb, grounded in the extended
`TicketSystem` / `CommandRunner` seam (§3.2). Both shell out only through the
injected `CommandRunner`, so both are fully unit-testable behind `FakeRunner`.

### 5.1 `tm issue seed-labels [--config <path>] [--dry-run]`

**Idempotent create-missing.** Ensures every state's label exists in the repo
with the configured color/description.

Algorithm:

1. Load + validate the YAML model (§6).
2. `list_repo_labels()` → set of existing label names.
3. For each `states[].label` not present, `create_label(...)` with its
   color/description.
4. Print a summary: `created: [...]`, `already present: [...]`.
5. `--dry-run` prints what *would* be created without calling `create_label`.

**Color/description drift on *existing* labels is left alone by default** (see
§9 open question 2). A future `--reconcile` flag may update drifted labels.

### 5.2 `tm issue transition <issue#> <to-state> [--config <path>] [--note <text>]`

**Validated, atomic state change + assignee application.**

Algorithm:

1. Load + validate the YAML model.
2. Resolve `<to-state>` to a known state; reject unknown target with a clear
   error listing valid states.
3. `validate(issue#)` (existing method) → fetch the `Issue` incl. its current
   `labels`. Determine the **current state** = the single state whose label is
   present on the issue. (Zero or multiple state-labels present is an error
   surfaced clearly — the visibility north star requires exactly one.)
4. Check the edge `current → to-state` is in `transitions[]`. If not, **reject**
   with: `invalid transition <from> → <to>; allowed from <from>: [...]`.
5. **Atomic label swap — single-call by default.** Swap the state label in **one
   `gh` invocation**:
   `gh issue edit <issue#> --add-label <to.label.name> --remove-label <from.label.name>`.
   `gh issue edit` accepts both `--add-label` and `--remove-label` in a single
   call, so the add and the remove are applied together — closing the window in
   which the issue could be observed carrying *both* labels (or *neither*). This
   directly serves the visibility north star (an issue is always in exactly one
   state from an observer's perspective) and also collapses the operation to a
   **single recorded `CommandRunner` call**, simplifying the test assertion to one
   `(program, args)` tuple. The two-call fallback (`add_label` then
   `remove_label`) is retained only as a documented degraded path for backends
   whose `edit` does not support a combined add/remove.
6. Apply the effective assignee rule for `to-state` from `assignee_model.per_state`
   (§4.1/§4.2) via `set_assignee(...)`. For the Unicorn Factory model **every
   non-initial state is `unchanged`** — the harness does **not** reassign during
   transitions, so this step is a no-op for the factory default; `set_assignee`
   exists for models that *do* mutate assignees per state. (The factory's
   `bot_identity` is a git-commit-attribution concern, not a GitHub-assignee one —
   see §4.1.)
7. Post a transition audit comment (visibility): `comment(issue#, "…")` recording
   `from → to` and any assignee change applied, plus any `--note` text. This keeps
   the transition reconstructable from comments even after labels change again.

> **Atomicity note (RESOLVED 2026-06-15: single-call is the default).** The
> single-call form `gh issue edit <n> --add-label <new> --remove-label <old>`
> applies the add and the remove together in one `gh` invocation, so there is no
> intermediate window in which the issue carries both labels or neither — the
> tightest atomicity `gh` offers, and the one most aligned with the visibility
> north star. It is therefore the **default** implementation. The two-call form
> (add-then-remove) is kept only as a documented **fallback** for backends without
> a combined-edit primitive; in that degraded mode the ordering is chosen so a
> mid-operation failure leaves the issue with *both* labels (recoverable, still
> clearly mid-transition) rather than *no* state label (ambiguous). The
> implementation PR documents the fallback ordering and its recovery story.

### 5.3 Supporting verbs (implementation may include)

- `tm issue states [--config <path>]` — print the configured states/transitions
  (operator introspection; reads YAML only, no `gh`).
- `tm issue current <issue#>` — print the issue's current state derived from its
  labels (read side; reconstructs state from GitHub artifacts).

### 5.4 Assignee application — exact `gh` semantics (incl. the `none` rule)

> **Note on the factory default.** The Unicorn Factory model's `assignee_model`
> is `bot_identity` with every per-state rule set to `unchanged` (assignees are set
> once at creation to `manifest.github.review_assignees`; the bot identity is used
> only for git commit attribution — §4.1). So for the factory default, `tm issue
> transition` performs **no** `set_assignee` call. The generic `self`/`bot`/`none`
> primitives below exist for *other* models that mutate assignees per state; they
> are not exercised by the factory default.

`set_assignee(issue, who)` maps three generic assignee rules to `gh issue
edit` invocations. The subtlety is the **`none` (clear-all) rule**: `gh issue
edit --remove-assignee` **requires an explicit login** — there is no
"clear all assignees" flag. Clearing therefore requires **reading the current
assignees first** and removing each by login.

| Rule   | Mechanism |
|--------|-----------|
| `self` | `gh api user --jq .login` → `<login>`, then `gh issue edit <n> --add-assignee <login>`. (Generic primitive; not used by the factory default, whose per-state rules are all `unchanged`.) |
| `bot`  | `gh issue edit <n> --add-assignee <bot_login>` for a configured bot login. (Generic primitive; **not** how the factory uses its `bob-unicorn` identity — that identity is git-attribution-only, never a GitHub assignee. §4.1.) |
| `none` | **Read then remove.** First read the issue's current assignees — reuse the existing `validate(issue#)` read (extend `Issue` to carry `assignees`, or query `gh issue view <n> --json assignees --jq '.assignees[].login'`). Then for each current assignee login `L`, `gh issue edit <n> --remove-assignee <L>`. As a shortcut, when the issue is known to be self-assigned, `gh issue edit <n> --remove-assignee @me` clears the authenticated user without a prior read. If there are no assignees, `none` is a no-op (no `gh` call). |

Because `--remove-assignee` needs a concrete login (or `@me`), the `none` rule is
**not** a single fixed command — its `gh` calls depend on the issue's current
assignee set, which the operation reads first. The `none` path is unit-tested by
scripting `gh issue view` (or the extended `validate`) to return a known assignee
set and asserting the exact `--remove-assignee <login>` calls (§7).

---

## 6. Config Discovery / Precedence

Aligned with the #1220 `~/.trusty-tools/<crate>/config.yaml` convention. `tm
issue` resolves the state-model YAML in this order (first hit wins):

1. **`--config <path>` flag** — explicit override (highest precedence).
2. **Project file** — `./issue-state.yaml` in the current working directory
   (per-repo customization, checkable into the consumer repo). The basename is
   deliberately the **same** as the user-config basename (`issue-state.yaml`) for
   consistency — the two differ only by location (CWD vs. `~/.trusty-tools/…`),
   not by name.
3. **User config** — `~/.trusty-tools/trusty-mpm/issue-state.yaml` (the #1220
   location).
4. **Embedded default** — the Unicorn Factory model compiled into the binary via
   `include_str!` (§2.3 / §4.2). Always available; nothing on disk required.

`tm issue seed-config [--force]` writes the embedded default to the user-config
path (mirrors `tm services` default-seeding, §2.3) so operators can start from a
copy and edit.

---

## 7. Testability

Every operation is unit-tested behind the existing `FakeRunner` (the scripted
`CommandRunner` from `ticket/system.rs`/`ticket/mod.rs`), with **no live `gh`**:

- **YAML schema:** `serde_yaml::from_str` round-trips for the default model
  (including `label_config`, `extra_labels`, the `null → queued` creation edge,
  the `trigger` annotations, and the `bot_identity` `assignee_model`); validation
  rejects: unknown `version`, duplicate state names, transition edges referencing
  missing states, non-hex colors, an unrecognised `assignee_model.strategy`.
- **State machine:** `transition_allowed(from, to)` true for every listed edge,
  false for an unlisted edge; unknown target state rejected with the
  valid-states list in the message.
- **`seed-labels`:** `FakeRunner` returns a scripted `gh label list` JSON missing
  some labels; assert only the *missing* ones trigger `gh label create` (recorded
  `(program, args)`), and present ones do not. `--dry-run` records **zero**
  `create` calls.
- **`transition`:** `FakeRunner` scripts `gh issue view` (current label present) →
  assert the **single** `gh issue edit --add-label <new> --remove-label <old>`
  call (the default single-call swap, §5.2). For the factory default the per-state
  rule is `unchanged`, so assert **no** `set_assignee`/`--add-assignee`/
  `--remove-assignee` call is recorded. A scripted issue whose current label has
  **no allowed edge** to the target asserts the operation **errors before any `gh`
  mutation** (no label-swap call recorded). Cover at least the canonical edges:
  `queued → approved`, `approved → active-development`, `active-development → done`,
  `active-development → failed`, and the halt edges `active-development → paused`/
  `→ blocked`; reject e.g. `done → active-development` (terminal) and
  `queued → done` (no edge). Zero/multiple state-labels present asserts a clear
  error.
- **Assignee rules (generic primitives, §5.4):** `self` → asserts a `gh api user`
  lookup then `--add-assignee <login>`; `bot` → `--add-assignee <bot_login>`;
  `none` → scripts a known current assignee set (via `gh issue view --json
  assignees` / extended `validate`) and asserts the exact `--remove-assignee
  <login>` call per current assignee (or a `--remove-assignee @me` shortcut when
  self-assigned); empty assignee set asserts **zero** `gh` calls (no-op). These
  primitives are exercised by tests for *non-factory* models; the factory default
  exercises only the `unchanged` path (no assignee mutation).

This mirrors the #1237/#1244 test convention exactly: scripted `CommandOutput`s,
recorded calls, assertions on `(program, args)`.

---

## 8. Backward-Compat / Migration (unicorn-factory #100)

The default YAML **must reproduce the current `github_client.py` behavior
end-to-end** (behavior-preserving). Migration path for bob-duetto/unicorn-factory
(ADR-0004 / #103 / child #100):

1. **Freeze the default YAML.** The committed
   `examples/issue-state/unicorn-factory.yaml` reproduces the **source-confirmed
   schema from #1246** (§2.5/§4.2) verbatim — every label name, color,
   description, the `null → queued` creation edge and all transition edges +
   triggers, and the `bot_identity` attribution-only assignee model. The
   implementation PR verifies the committed YAML matches that schema exactly (a
   mechanical equality check against a known-good schema; no longer a discovery
   exercise against inaccessible source). This is a hard acceptance gate.
2. **Adopt in the harness (#100).** Replace the hardcoded state machine in
   `github_client.py` with shell-outs to `tm issue`:
   - label seeding → `tm issue seed-labels`,
   - state changes → `tm issue transition <issue#> <to-state>`.
   The harness keeps owning *when* to transition; trusty-mpm owns *how*.
3. **Visibility unchanged.** Because every operation maps to label/assignee/comment
   mutations on the issue, the north star (state reconstructable from GitHub
   artifacts alone) is preserved — in fact strengthened by the audit comment in
   §5.2.

No trusty-mpm behavior is removed or changed for existing users: `tm issue` is an
**additive** verb group; `tm ticket` and all other verbs are untouched.

---

## 9. Phased Plan + Acceptance Criteria

### Phase table

| Phase | Description | Size | Backward compat |
|---|---|---|---|
| P1 | YAML schema + `config.rs` (load/discovery/validate) + `state.rs` (state machine) + embedded default; `tm issue states` / `seed-config`. | M | Additive |
| P2 | Extend `TicketSystem`/`GhTicketSystem` with label/assignee methods; `tm issue seed-labels` (idempotent, `--dry-run`). | M | Additive |
| P3 | `tm issue transition` (edge validation, atomic label swap, assignee rule, audit comment); `tm issue current`. | M | Additive |
| P4 | Verify default YAML matches the source-confirmed #1246 schema; wire unicorn-factory #100 to shell out to `tm issue`. | S | Additive (cross-repo) |

### Acceptance criteria (mirroring #1246)

- [ ] A **documented YAML schema** with a committed example/default
      (`crates/trusty-mpm/examples/issue-state/unicorn-factory.yaml`).
- [ ] Label seeding, transition validation, and assignee application are **driven
      entirely by the YAML** — no state names, label strings, or identities
      hardcoded in source (only the *embedded default YAML text* via `include_str!`).
- [ ] The **default YAML reproduces the factory's label/assignee behavior
      end-to-end** — it transcribes the **source-confirmed schema appended to
      #1246** verbatim (full label families, `queued`-initial lifecycle with no
      `in-review` state, transition edges + triggers, and the attribution-only
      `bot_identity` assignee model; §2.5/§4.2).
- [ ] **Invalid transitions** (edges not in the YAML) are **rejected with a clear
      error** before any `gh` mutation.
- [ ] All operations **unit-tested behind `FakeRunner`** (no live `gh`);
      `cargo test -p trusty-mpm`, `cargo clippy --workspace -- -D warnings`, and
      `cargo fmt --check` pass.
- [ ] New source files stay under the **500-SLOC production cap** (split
      `commands/issue/` into `config.rs` / `state.rs` / `ops.rs` / `mod.rs`).
- [ ] **Visibility guarantee:** every operation mutates GitHub artifacts
      (labels/assignee/comment) so state is reconstructable from GitHub alone.
- [ ] **Trait extension does not break the stub backends:** the six new
      `TicketSystem` methods ship with **default erroring bodies** (§3.3), and
      `Jira`/`Linear` keep compiling unchanged (`cargo check -p trusty-mpm`).
- [ ] **`transition` uses the single-call label swap by default** (§5.2):
      `gh issue edit … --add-label … --remove-label …`, asserted as one recorded
      `CommandRunner` call.
- [ ] **`serde_yaml` migration tracking issue is filed** (PM) to move the
      workspace to a maintained YAML crate (`serde_yml`/`serde-yaml-ng`) **before
      this subsystem ships to production** (unicorn-factory #100 go-live). This RFC
      does not change the dependency; it records the dependency on that follow-up.

---

## 10. Open Questions & Resolutions

The placement, surface, and format questions are **locked owner decisions**
stated in §1.2. At owner sign-off (2026-06-15) three of the prior open questions
were **resolved**; two genuine items remain open, each with the RFC's recommended
default noted.

### 10.1 Resolved at sign-off (2026-06-15)

1. **`serde_yaml` successor — RESOLVED 2026-06-15.** `serde_yaml 0.9` is
   unmaintained. **Decision:** reuse it as-is for this RFC (no new surface; §2.4),
   **and** file a separate follow-up tracking issue to migrate the whole workspace
   to a maintained YAML crate (`serde_yml` / `serde-yaml-ng`) **before this
   subsystem ships to production** (unicorn-factory #100 go-live). The PM is filing
   that tracking issue. This RFC depends on that migration but does not perform it.

2. **Transition atomicity primitive — RESOLVED 2026-06-15 in favor of
   single-call.** **Decision:** the **single-call** form
   `gh issue edit <n> --add-label <new> --remove-label <old>` is the **default**
   (§5.2) — it applies both mutations together (closing the both-labels/no-label
   window, directly serving the visibility north star) and collapses the test
   assertion to one recorded `CommandRunner` call. The two-call add-then-remove
   form is retained only as a documented fallback for backends without a combined
   edit.

3. **Exact Unicorn Factory model values — RESOLVED 2026-06-15 (source-confirmed).**
   The owner appended the **exact, source-extracted schema** to issue #1246
   (derived from `github_client.py` / `executor.py` / `ticket_builder.py` /
   `manifest.py`, with per-rule line references). **Decision:** the default YAML
   transcribes that authoritative schema verbatim (§2.5/§4.2): states
   `queued` (initial) → `approved` → `active-development` → `done`, plus halt
   states `paused`/`blocked` branching from `active-development` and the terminal
   `failed`; **there is no `in-review` state**; the full `unicorn`/`blast:*`/
   `T2-T4`/`approval:level-*` label families with exact colors; and an
   attribution-only `bot_identity` model (issues assigned to
   `manifest.github.review_assignees`, the `bob-unicorn` identity used only for git
   commit attribution). This **supersedes** the earlier "assumed model" (which
   wrongly used `approved` as the initial state and included a non-existent
   `in-review` state). The implementation gate is now a **mechanical equality check
   against this known-good schema**, not a discovery exercise; behavior-preserving
   adoption (§8, §9) is unblocked.

### 10.2 Still open (recommended defaults noted)

1. **Label color/description drift on *existing* labels.** When `seed-labels`
   finds a state's label already present but with a **different color or
   description** than the YAML, should it (a) leave it alone, (b) reconcile it to
   match the YAML (overwrite), or (c) warn only?
   **RFC recommended default:** (a) **leave existing labels alone**, and add an
   opt-in `--reconcile` flag later to overwrite drifted color/description. This
   keeps `seed-labels` purely create-missing and non-destructive by default.

2. **Multi-identity / bot auth.** The generic `self` rule assigns the authenticated
   `gh` user; the generic `bot` rule assigns a configured `bot_login` by name
   (§5.4). The **Unicorn Factory does not use either** for issue assignment — its
   `assignee_model` is `bot_identity` with every per-state rule `unchanged`: issue
   assignees are the human reviewers (`manifest.github.review_assignees`), and the
   `bob-unicorn` identity is used **only for git commit attribution** (worktree-local
   `user.name`/`user.email` + `Unicorn:`/`Unicorn-Issue:` trailers — §4.1). So no
   GitHub-identity switching by `tm issue` is needed for the factory. Is there a
   near-term need for `tm issue` to *switch* gh identity itself (e.g. a token /
   `--as` flag) for *other* models?
   **RFC recommended default:** **no in-process identity switching** for now. The
   factory needs none (attribution is handled in git config, not via `tm`).
   Revisit only if a future non-factory consumer needs a single `tm` invocation to
   act as multiple GitHub assignee identities.

---

## 11. Reference: Key Files

| File | Relevance |
|---|---|
| `crates/trusty-mpm/src/bin/tm/commands/ticket/runner.rs` | `CommandRunner` / `CommandOutput` / `RealCommandRunner` — the process seam to reuse |
| `crates/trusty-mpm/src/bin/tm/commands/ticket/system.rs` | `TicketSystem` trait, `GhTicketSystem`, `Issue`, `FakeRunner` test pattern — the trait to extend |
| `crates/trusty-mpm/src/bin/tm/commands/ticket/mod.rs` | `tm ticket` dispatcher — the orchestration pattern to mirror |
| `crates/trusty-mpm/src/bin/tm/cli.rs` | `Command` enum / subcommand definitions — where `Issue { … }` is added |
| `crates/trusty-mpm/src/bin/tm/main.rs` | Subcommand dispatch — where the `Command::Issue` arm is added |
| `crates/trusty-mpm/src/bin/tm/commands/mod.rs` | Module registration — where `pub(crate) mod issue;` is added |
| `crates/trusty-mpm/src/services/manifest.rs` | `DEFAULT_MANIFEST_YAML` + `include_str!` embedded-default precedent |
| `crates/trusty-mpm/src/bin/tm/commands/services.rs` | Seed-default-to-disk precedent (`serde_yaml::to_string`) |
| `crates/trusty-mpm/src/core/error.rs` | Existing `#[from] serde_yaml::Error` thiserror variant |
| `crates/trusty-mpm/Cargo.toml` | `serde_yaml.workspace = true` (already present) |
| `docs/trusty-mpm/design/RFC-session-manager-mcp-console.md` | Sibling RFC; format/convention this doc mirrors |
| (cross-repo) `bob-duetto/unicorn-factory` `src/unicorn/{github_client,executor,ticket_builder,manifest}.py` | The hardcoded model the default YAML reproduces; the exact source-extracted schema (with line references) is in #1246 / §2.5 / §4.3 |
| #1246 appended schema ("Concrete schema — current Unicorn Factory model") | The authoritative, source-confirmed model transcribed into §4.2/§4.3 |
| (cross-repo) unicorn-factory ADR-0004 / #100 / #103 | Migration driver; #100 is the first consumer unblocked by this RFC |
