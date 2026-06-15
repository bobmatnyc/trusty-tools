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
`src/unicorn/github_client.py`:

- the **label set** that represents each state,
- the **allowed label transitions** (`approved → active-development → … → done`),
- the **assignee / identity model** (self-assign under the `bob-unicorn` identity),
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
  (behavior-preserving migration).
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

### 2.5 The hardcoded Unicorn Factory model (source NOT accessible)

> **Decision (RESOLVED 2026-06-15): adopt the assumed model now, reconcile at
> implementation time as a hard acceptance gate.** `src/unicorn/github_client.py`
> **could not be read** at authoring time — the repo `bob-duetto/unicorn-factory`
> returns HTTP 404 under the active `gh` token (`bobmatnyc` account; the
> `bob-duetto` account is logged in but not active and has no access to the repo),
> and `gh search code --owner bob-duetto` returned nothing. The owner has
> **decided** to proceed on the **assumed model below** (states `approved →
> active-development → in-review → done`, plus `blocked`/`failed`; self-assign
> under the `bob-unicorn` bot identity) as the working default. This is not an
> open question — it is the accepted starting point. The label names, colors,
> descriptions, and exact transition edges remain marked **"TO BE CONFIRMED
> against `github_client.py` during unicorn-factory #100 adoption"**, and the
> implementation PR **must** diff the committed default YAML against the real
> `github_client.py` and reconcile any drift. **That reconciliation remains a hard
> acceptance-criteria gate (§8, §9).** Adopting the assumed model now does not
> relax the gate — it only removes the model choice from the list of open
> questions.

From the issue body, the current model is (TO BE CONFIRMED):

- **States:** `approved → active-development → in-review → done`, plus `failed`
  and `blocked` as off-path terminal/holding states.
- **Labels:** one GitHub label per state. Exact label strings, colors, and
  descriptions are **unknown from source** — the issue references e.g. an
  `active-development` label and a (possibly `unicorn:`-prefixed) naming scheme
  (`unicorn:approved`). The default YAML records placeholder values flagged for
  confirmation.
- **Assignee/identity:** self-assign under the `bob-unicorn` bot identity.
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

### 4.1 Schema overview

| Key                         | Type            | Required | Meaning |
|-----------------------------|-----------------|----------|---------|
| `version`                   | integer         | yes      | Schema version (start at `1`). |
| `identity.default`          | enum            | yes      | Global default assignee rule: `self`, `bot`, or `none`. |
| `identity.bot_login`        | string          | when `bot` used | Bot login for `bot` rule (e.g. `bob-unicorn`). |
| `states[]`                  | list            | yes      | Ordered list of states. |
| `states[].name`             | string          | yes      | Human/machine state name (unique key; used by `tm issue transition`). |
| `states[].order`            | integer         | no       | Display/sort ordering (informational; does not gate transitions). |
| `states[].label.name`       | string          | yes      | GitHub label representing this state (the visible artifact). |
| `states[].label.color`      | string (hex)    | yes      | 6-hex-digit color, no `#`. Used by `seed-labels`. |
| `states[].label.description`| string          | no       | Label description used by `seed-labels`. |
| `states[].assignee`         | enum            | no       | Per-state override: `self` \| `bot` \| `none`. Falls back to `identity.default`. |
| `transitions[]`             | list            | yes      | Allowed `from → to` edges. |
| `transitions[].from`        | string          | yes      | Source state name (must match a `states[].name`). |
| `transitions[].to`          | string          | yes      | Destination state name (must match a `states[].name`). |

**Assignee rule semantics:**

- `self` — assign the **current `gh` authenticated user** (`gh api user --jq .login`).
  This is what reproduces the harness's "self-assign" behavior when `tm` runs
  authenticated as the bot.
- `bot` — assign the explicit `identity.bot_login` (e.g. `bob-unicorn`),
  regardless of who is authenticated.
- `none` — clear all assignees on the issue.

**Validation rules (enforced at load, before any `gh` call):**

1. `version` is recognised (`1`).
2. Every `states[].name` is unique and non-empty.
3. Every `transitions[].from` / `.to` references an existing state name.
4. Every `states[].label.color` is a 6-hex-digit string.
5. If any effective assignee rule is `bot`, `identity.bot_login` is present.
6. The transition graph is well-formed (no edge references a missing state).

### 4.2 Complete annotated example (default = Unicorn Factory model)

> Committed at `crates/trusty-mpm/examples/issue-state/unicorn-factory.yaml` and
> embedded in the binary via `include_str!` as the default. **All label strings,
> colors, and descriptions below are TO BE CONFIRMED against
> `src/unicorn/github_client.py` (§2.5) before the default is frozen.**

**Entry state: `approved` is set externally, not via `tm issue transition`.**
The state machine deliberately has **no entry edge into `approved`** — there is
no `transitions[]` row whose `to` is `approved`. `approved` is the **initial
state**, applied **externally** by manual labeling / triage (a human or an
upstream triage step adds the `approved` label to an issue). `tm issue
transition` only moves an issue **between** existing states along the listed
edges; it never *creates* the initial state. This matches the Unicorn Factory
model, where an issue becomes a candidate for autonomous work the moment a human
approves it (labels it `approved`), and the first thing the harness does is
transition it `approved → active-development`. Operationally: `tm issue
seed-labels` ensures the `approved` label *exists* in the repo; a human/triage
applies it; the unicorn then drives the machine forward from there. An issue with
**no** state label is "not yet in the machine" (pre-`approved`) and is simply not
acted upon by `tm issue transition` until it carries a recognised state label.

```yaml
# trusty-mpm issue-state model — Unicorn Factory default.
# This file is the portable contract between the Python harness and `tm issue`.
# State is reconstructable from GitHub artifacts alone: the label on an issue IS
# the state; the assignee is the identity; comments record the transition audit.
version: 1

identity:
  # Global default applied to any state that does not override `assignee`.
  # `self`  -> assign the authenticated gh user (the bot, when tm runs as the bot)
  # `bot`   -> assign `bot_login` explicitly
  # `none`  -> clear assignees
  default: self
  # The named bot identity for `bot` rules. TO BE CONFIRMED: `bob-unicorn`.
  bot_login: bob-unicorn

states:
  - name: approved              # INITIAL state — set externally by triage/manual
                                # labeling; has NO entry edge in transitions[].
    order: 10
    label:
      name: approved            # TO BE CONFIRMED (may be `unicorn:approved`)
      color: 0e8a16             # green — TO BE CONFIRMED
      description: "Approved for autonomous work; not yet started."
    assignee: self              # the unicorn self-assigns on pickup

  - name: active-development     # the unicorn is actively working
    order: 20
    label:
      name: active-development  # TO BE CONFIRMED
      color: 1d76db             # blue — TO BE CONFIRMED
      description: "A unicorn is actively implementing this issue."
    assignee: self

  - name: in-review             # PR open, awaiting review/merge
    order: 30
    label:
      name: in-review           # TO BE CONFIRMED
      color: fbca04             # yellow — TO BE CONFIRMED
      description: "Implementation complete; PR open and under review."
    assignee: self

  - name: done                  # terminal success
    order: 40
    label:
      name: done                # TO BE CONFIRMED
      color: 5319e7             # purple — TO BE CONFIRMED
      description: "Work merged and complete."
    assignee: none              # release the assignee on completion (TO BE CONFIRMED)

  - name: blocked               # holding state; can return to active
    order: 50
    label:
      name: blocked             # TO BE CONFIRMED
      color: d93f0b             # orange-red — TO BE CONFIRMED
      description: "Work cannot proceed; awaiting external unblock."
    assignee: self

  - name: failed                # terminal failure
    order: 60
    label:
      name: failed              # TO BE CONFIRMED
      color: b60205             # red — TO BE CONFIRMED
      description: "Autonomous work failed; needs human intervention."
    assignee: none              # TO BE CONFIRMED

# Allowed edges. Anything not listed here is rejected by `tm issue transition`.
transitions:
  - { from: approved,           to: active-development }
  - { from: active-development,  to: in-review }
  - { from: in-review,          to: done }
  # Off-path edges (TO BE CONFIRMED against github_client.py):
  - { from: active-development,  to: blocked }
  - { from: blocked,            to: active-development }
  - { from: active-development,  to: failed }
  - { from: in-review,          to: active-development }   # review bounce-back
  - { from: in-review,          to: failed }
```

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
6. Apply the effective assignee rule for `to-state`
   (`states[].assignee` ?? `identity.default`) via `set_assignee(...)`.
7. Post a transition audit comment (visibility): `comment(issue#, "…")` recording
   `from → to` and the assignee applied, plus any `--note` text. This keeps the
   transition reconstructable from comments even after labels change again.

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

`set_assignee(issue, who)` maps the three assignee rules (§4.1) to `gh issue
edit` invocations. The subtlety is the **`none` (clear-all) rule**: `gh issue
edit --remove-assignee` **requires an explicit login** — there is no
"clear all assignees" flag. Clearing therefore requires **reading the current
assignees first** and removing each by login.

| Rule   | Mechanism |
|--------|-----------|
| `self` | `gh api user --jq .login` → `<login>`, then `gh issue edit <n> --add-assignee <login>`. (When `tm` runs authenticated as the bot, this self-assigns the bot — reproducing the harness behavior.) |
| `bot`  | `gh issue edit <n> --add-assignee <identity.bot_login>` (e.g. `bob-unicorn`). |
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

- **YAML schema:** `serde_yaml::from_str` round-trips for the default model;
  validation rejects: unknown `version`, duplicate state names, transition edges
  referencing missing states, non-hex colors, `bot` rule without `bot_login`.
- **State machine:** `transition_allowed(from, to)` true for every listed edge,
  false for an unlisted edge; unknown target state rejected with the
  valid-states list in the message.
- **`seed-labels`:** `FakeRunner` returns a scripted `gh label list` JSON missing
  some labels; assert only the *missing* ones trigger `gh label create` (recorded
  `(program, args)`), and present ones do not. `--dry-run` records **zero**
  `create` calls.
- **`transition`:** `FakeRunner` scripts `gh issue view` (current label present) →
  assert the **single** `gh issue edit --add-label <new> --remove-label <old>`
  call (the default single-call swap, §5.2) and the correct `set_assignee` call,
  in order. A scripted issue whose current label has **no allowed edge** to the
  target asserts the operation **errors before any `gh` mutation** (no label-swap
  call recorded). Zero/multiple state-labels present asserts a clear error.
- **Assignee rules:** `self` → asserts a `gh api user` lookup then `--add-assignee
  <login>`; `bot` → `--add-assignee bob-unicorn`; `none` → scripts a known current
  assignee set (via `gh issue view --json assignees` / extended `validate`) and
  asserts the exact `--remove-assignee <login>` call per current assignee (or a
  `--remove-assignee @me` shortcut when self-assigned); empty assignee set asserts
  **zero** `gh` calls (no-op) — see §5.4.

This mirrors the #1237/#1244 test convention exactly: scripted `CommandOutput`s,
recorded calls, assertions on `(program, args)`.

---

## 8. Backward-Compat / Migration (unicorn-factory #100)

The default YAML **must reproduce the current `github_client.py` behavior
end-to-end** (behavior-preserving). Migration path for bob-duetto/unicorn-factory
(ADR-0004 / #103 / child #100):

1. **Freeze the default YAML.** The implementation PR diffs
   `examples/issue-state/unicorn-factory.yaml` against the real
   `src/unicorn/github_client.py` (§2.5) and reconciles every label name, color,
   description, transition edge, and the `bob-unicorn` self-assign rule. This is a
   hard acceptance gate.
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
| P4 | Reconcile default YAML against real `github_client.py`; wire unicorn-factory #100 to shell out to `tm issue`. | S | Additive (cross-repo) |

### Acceptance criteria (mirroring #1246)

- [ ] A **documented YAML schema** with a committed example/default
      (`crates/trusty-mpm/examples/issue-state/unicorn-factory.yaml`).
- [ ] Label seeding, transition validation, and assignee application are **driven
      entirely by the YAML** — no state names, label strings, or identities
      hardcoded in source (only the *embedded default YAML text* via `include_str!`).
- [ ] The **default YAML reproduces the factory's label/assignee behavior
      end-to-end** (reconciled against `github_client.py`).
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

3. **Exact Unicorn Factory model values — RESOLVED 2026-06-15.** `github_client.py`
   was inaccessible at authoring time (§2.5). **Decision:** proceed on the
   **assumed model** (states `approved → active-development → in-review → done`,
   plus `blocked`/`failed`; self-assign under `bob-unicorn`) as the working
   default, with `approved` as the externally-set initial state (§4.2). The exact
   label strings/colors/descriptions/edges remain marked **"to be confirmed
   against `github_client.py` during #100 adoption"**, and reconciling the
   committed default YAML against the real source **remains a hard
   acceptance-criteria gate** (§8 step 1, §9). Adopting the assumed model removes
   the *model choice* from the open list without relaxing the reconciliation gate.

### 10.2 Still open (recommended defaults noted)

1. **Label color/description drift on *existing* labels.** When `seed-labels`
   finds a state's label already present but with a **different color or
   description** than the YAML, should it (a) leave it alone, (b) reconcile it to
   match the YAML (overwrite), or (c) warn only?
   **RFC recommended default:** (a) **leave existing labels alone**, and add an
   opt-in `--reconcile` flag later to overwrite drifted color/description. This
   keeps `seed-labels` purely create-missing and non-destructive by default.

2. **Multi-identity / bot auth.** The `self` rule assigns the authenticated `gh`
   user; the `bot` rule assigns `identity.bot_login` by name. For the Unicorn
   Factory the harness runs `tm` authenticated **as** `bob-unicorn`, so `self`
   reproduces current behavior. Is there a near-term need for `tm issue` to
   *switch* gh identity itself (e.g. a token / `--as` flag)?
   **RFC recommended default:** **no in-process identity switching** for now — run
   `tm` under the bot's auth (the `self` rule then self-assigns the bot). Revisit
   only if a consumer needs a single `tm` invocation to act as multiple identities.

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
| (cross-repo) `bob-duetto/unicorn-factory` `src/unicorn/github_client.py` | The hardcoded model the default YAML must reproduce (inaccessible at authoring — §2.5) |
| (cross-repo) unicorn-factory ADR-0004 / #100 / #103 | Migration driver; #100 is the first consumer unblocked by this RFC |
