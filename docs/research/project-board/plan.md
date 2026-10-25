# Every repo adopting trusty-mpm gets a standard GitHub Projects board, created from nothing, and tm adds workflow issues to it

Plan document for the portable project board epic. The `tm-epic` skill governs
the tracker and phase shape. `tm issue epic create --from` reads the
`## Epic plan` section; the H1 above becomes the tracker's outcome. Filing
metadata the parser does not read sits under `## Filing` at the end.

## Context

trusty-tools (trusty-mpm, `tm`) is used by many developers across many
unrelated repositories owned by users or organisations on github.com, including
GitHub Enterprise Cloud. Today only one board exists in the standard format:
Project [#45](https://github.com/users/bobmatnyc/projects/45), `bobmatnyc`'s
`trusty-tools-master`. A repo that adopts the workflow has no way to get a
board in that format except by hand, and no `tm` code adds an agent-filed
issue to a board.

After this epic, each adopting repo gets a board in a standard format owned by
trusty-tools and created from nothing — never copied from [#45](https://github.com/users/bobmatnyc/projects/45). Issues opened
through the trusty-mpm workflow are added to the repo's board by code.
Ticketing behaviour already in the trusty-tools repo does not change.

**Reference board.** Only [#45](https://github.com/users/bobmatnyc/projects/45). No other board is a reference.

**Out of scope.** GitHub Enterprise Server (self-hosted); any change to [#45](https://github.com/users/bobmatnyc/projects/45);
Status writes on transition
([#9400](https://github.com/bobmatnyc/trusty-tools/issues/9400)); turning
Hold on ([#9401](https://github.com/bobmatnyc/trusty-tools/issues/9401));
migrating authority from labels to the Status field
([#9402](https://github.com/bobmatnyc/trusty-tools/issues/9402)); the
trusty-mpm daemon bug report and the trusty-agents ticketing client as issue
creators; pull requests as board items (issues only).

### Relationship to the Status-field epic

This is a standalone epic, not a phase of [#9397](https://github.com/bobmatnyc/trusty-tools/issues/9397). [#9400](https://github.com/bobmatnyc/trusty-tools/issues/9400) is blocked-by this
epic. Two pieces of ownership move here; the moves are recorded by
comments on the issues named, and no issue body is edited:

- Status option remapping moves from [#9402](https://github.com/bobmatnyc/trusty-tools/issues/9402) (its `migrate status` verb) to this
  epic's `tm issue board apply`.
- [#9400](https://github.com/bobmatnyc/trusty-tools/issues/9400) uses this epic's Projects module, its `project:` schema and its
  option-to-label mapping.

Coordination comments are posted when the epic is filed — on [#9397](https://github.com/bobmatnyc/trusty-tools/issues/9397), [#9400](https://github.com/bobmatnyc/trusty-tools/issues/9400),
[#9401](https://github.com/bobmatnyc/trusty-tools/issues/9401) (including the correction that Hold already exists on [#45](https://github.com/users/bobmatnyc/projects/45)) and [#9402](https://github.com/bobmatnyc/trusty-tools/issues/9402) —
and again as each phase's closing step.

## Research findings

Verified against the checkout at the time of writing; line numbers rot, so
re-verify before editing.

- `tm` code never reads `TICKETING.md`; the ticketing agent does. The
  `tm issue` verbs read `issue-state.yaml` and `agents.ticketing` in
  `~/.trusty-tools/trusty-mpm/config.yaml`.
- One `tm` code path creates an issue: `tm issue epic create`
  (`crates/trusty-mpm/src/bin/tm/commands/issue/epic/backend.rs:438-463`). It
  already attaches the issue to a project through `attach_project_or_waive`
  (`crates/trusty-mpm/src/bin/tm/commands/issue/epic/create.rs:350-384`),
  which runs `gh project item-add` (`epic/backend.rs:619-638`). Every other
  workflow issue is created by the ticketing agent with a raw
  `gh issue create` and attached by instruction. The audit's project row passes
  on any board (`crates/trusty-mpm/src/core/issue_audit.rs:331-361`). No
  production code path in the crate calls `gh api graphql`; the only GraphQL
  mentions are doc comments and the pm-guard read-only classifier.
- `tm pr open` adds a PR to the `agents.ticketing.default_project` board
  (`crates/trusty-mpm/src/bin/tm/commands/pr/open.rs:702`).
- `status-lifecycle.yaml` is compiled into `tm` with `include_str!`
  (`crates/trusty-mpm/src/bin/tm/commands/issue/config.rs:43`); the board
  spec follows the same pattern.
- [#45](https://github.com/users/bobmatnyc/projects/45) fields (REST read): Status with 9 options, each with a colour and a
  description — Backlog GRAY, Ready for Development BLUE, In Development
  YELLOW, In PR ORANGE, On Preview YELLOW, Merged GREEN, On Prod PINK,
  Published PURPLE, Won't Do GRAY; Crate (11 options); Hold (Blocked RED,
  Needs decision ORANGE); dates Deployed on, Published on, Start date, Target
  date; text field Agents. [#45](https://github.com/users/bobmatnyc/projects/45)'s README uses older stage names.
- GitHub API: project create, single-select fields with colours and
  descriptions, option edits and repo linking are reproducible through
  GraphQL. Built-in workflows (auto-add) are read/delete only; enabling one
  needs the UI or the `actions/add-to-project` Action, which needs a PAT or
  App token — `GITHUB_TOKEN` cannot write Projects. Templates are org-only.
  Whether views can be created through the API is DISPUTED between reviews;
  the Phase 2 trial settles it. Whether option IDs and item values survive an
  option edit is UNPROVEN; the Phase 2 trial settles it. User-owned boards
  need a classic PAT. Token scopes show in the `X-OAuth-Scopes` response
  header; an SSO failure shows as a 403 with an `X-GitHub-SSO` header
  (assumed, not yet observed).

## Preparation

Before Phase 1, no code:

1. A writing agent captures [#45](https://github.com/users/bobmatnyc/projects/45)'s views and enabled workflows through GraphQL
   into `docs/research/project-board/`.
2. This plan is merged to `origin/main`.
3. `ticketing` files the epic and its phases with the blocked-by link from
   [#9400](https://github.com/bobmatnyc/trusty-tools/issues/9400) and the coordination comments listed under Context.
4. The model-usage-monitor clone is fast-forwarded.

## Epic plan

trusty-mpm is used across many unrelated GitHub repositories, but only one board ([#45](https://github.com/users/bobmatnyc/projects/45)) exists in the standard format and no `tm` code adds an agent-filed issue to a board. After this epic, `tm issue board init` creates a conformant board from nothing on any user or organisation on github.com including GitHub Enterprise Cloud, `tm issue board add` puts workflow issues on the repo's board, `tm issue board check` reports drift, and the ticketing agent and the Architect provisioning checklist call those commands. Ticketing behaviour already in the trusty-tools repo does not change.

### Outcomes

- **O1** `tm issue board init --owner <org|user> --repo <owner/repo> --apply` on an owner with no same-title or repo-linked project creates a GitHub Projects v2 board carrying the standard fields of `board-v1.yaml` — Status with the 9 options, colours and descriptions of [#45](https://github.com/users/bobmatnyc/projects/45); Hold declared and off; the four date fields — links the repo, and prints the `project:` block for `issue-state.yaml`. Nothing is copied from [#45](https://github.com/users/bobmatnyc/projects/45).
- **O2** Every issue opened through the trusty-mpm workflow lands on the repo's board by code: `tm issue epic create` through the shared Projects module, and the ticketing agent through `tm issue board add <N>` after each `gh issue create`.
- **O3** trusty-tools' own ticketing behaviour is unchanged: TICKETING.md rules, `status:*` labels and [#45](https://github.com/users/bobmatnyc/projects/45) as they are today. `tm issue board check` run against [#45](https://github.com/users/bobmatnyc/projects/45) with trusty-tools' `project:` block reports no drift, with Crate and Agents reported as repo extras.
- **O4** model-usage-monitor (duettoresearch org) has a live board created by `init`, its open issues backfilled with Status set from labels under the explicit flag, and `check` clean.

### Ratified decisions

| # | Decision |
|---|---|
| D1 | The board spec is the single source of field and option names. Option names, not IDs, are the stable keys. [#9400](https://github.com/bobmatnyc/trusty-tools/issues/9400)'s `project:` block uses the same schema. |
| D2 | One Projects code module, `crates/trusty-mpm/src/bin/tm/commands/issue/board/`, which [#9400](https://github.com/bobmatnyc/trusty-tools/issues/9400) and [#9401](https://github.com/bobmatnyc/trusty-tools/issues/9401) extend later. |
| D3 | No Status writes on a live repo by default. Setting Status from labels needs an explicit flag. |
| D4 | Hold and the date fields are declared in the spec. Hold stays switched off until [#9401](https://github.com/bobmatnyc/trusty-tools/issues/9401) turns it on. A field that exists on a board but is declared off counts as conformant. |
| D5 | Only [#45](https://github.com/users/bobmatnyc/projects/45) is the reference board. Every board is created from nothing through the API; nothing is copied from [#45](https://github.com/users/bobmatnyc/projects/45) and [#45](https://github.com/users/bobmatnyc/projects/45) is never changed. |
| D6 | Standard fields: Status (the 9 options with [#45](https://github.com/users/bobmatnyc/projects/45)'s colours and descriptions), Hold, Deployed on, Published on, Start date, Target date. Not standard: Crate, Agents, any component field — component stays in labels. trusty-tools' `project:` block declares Crate and Agents as repo extras. |
| D7 | Issues are board items; pull requests are not. `tm pr open` stops using `default_project` to add PRs to a board. |
| D8 | The `project:` block in `issue-state.yaml` is the single attach target. It wins over TICKETING.md and over the per-machine `agents.ticketing.default_project` title, which stays only as a fallback for repos without a block; `check` reports a mismatch. |
| D9 | GitHub only. `tm issue board` refuses on a non-GitHub tracker. GitHub Enterprise Server is out of scope. |
| D10 | Attach failure follows the `epic create` precedent: a missing-scope refusal posts one `no-project:` comment and exits 0; any other error (502, SSO 403, deleted project) exits non-zero with the issue already filed. |
| D11 | Standalone epic. [#9400](https://github.com/bobmatnyc/trusty-tools/issues/9400) is blocked-by this epic. Status option remapping moves from [#9402](https://github.com/bobmatnyc/trusty-tools/issues/9402)'s `migrate status` to `tm issue board apply`; [#9400](https://github.com/bobmatnyc/trusty-tools/issues/9400) uses this epic's module, `project:` schema and option-to-label mapping. The moves are recorded by comments, never by editing an issue body. |
| D12 | No ticketing-text changes in this epic. [#9399](https://github.com/bobmatnyc/trusty-tools/issues/9399) owns TICKETING.md and skeleton wording. |

### Ordering

Phase 2 builds commands on the spec Phase 1 ships: `init` creates the fields
`board-v1.yaml` declares, `check` diffs a live board against it, and `add`
reads the `project:` block Phase 1 adds to `issue-state.yaml`. None of that
exists until Phase 1 is merged. Phase 2 also opens with a trial on a
throwaway project that settles two unproven API questions (option-edit
survival; view creation); `apply`'s option-edit behaviour and the views
fallback decision depend on its result, so the trial cannot run before Phase
2 starts and Phase 2 cannot be designed around its answer in advance. Phase 3
wires the ticketing agent and the Architect checklist to `tm issue board add`
and `check`; an instruction that names a command no released `tm` carries
would send every agent into a refusal, so Phase 3 waits for Phase 2 to merge.

<!-- phases:start -->
| # | Phase | Issue | State | Gate |
|---|-------|-------|-------|------|
| 1 | board-v1.yaml spec and the issue-state.yaml project block | TBD | open | [#45](https://github.com/users/bobmatnyc/projects/45) views and workflows captured under docs/research/project-board/; this plan on origin/main |
| 2 | tm issue board init, apply, check, show and add on one Projects module | TBD | open | Phase 1 merged |
| 3 | ticketing agent and Architect checklist call tm issue board | TBD | open | Phase 2 merged |
<!-- phases:end -->

### Phase: board-v1.yaml spec and the issue-state.yaml project block

Gate: [#45](https://github.com/users/bobmatnyc/projects/45) views and workflows captured under `docs/research/project-board/`; this plan on `origin/main`.

Ships the board spec as data compiled into `tm`, and the `project:` block
schema in `issue-state.yaml`. No command reads the spec yet; that is Phase 2,
so this phase is reviewed and reverted on its own. Rung 3: localized change in
the trusty-mpm crate. Decisions D1, D4, D5, D6 and D12 are in the tracker.

#### Scope

- `crates/trusty-mpm/examples/issue-state/board-v1.yaml`, compiled into `tm` with `include_str!` the way `status-lifecycle.yaml` is (`crates/trusty-mpm/src/bin/tm/commands/issue/config.rs:43` at the time of writing), with a `version` field.
- Standard fields: Status with the 9 options and [#45](https://github.com/users/bobmatnyc/projects/45)'s colours and descriptions; Hold (Blocked RED, Needs decision ORANGE) declared with `enabled: false`; Deployed on, Published on, Start date, Target date.
- Views, taken from the [#45](https://github.com/users/bobmatnyc/projects/45) capture; the auto-add intent; a README template.
- Each Status option maps to an `issue-state.yaml` state or declares `label: none`.
- `issue-state.yaml` gains the `project:` block schema: owner, project number, options to drop, extra fields. trusty-tools' own block declares Crate and Agents as repo extras.
- Changelog fragment under `crates/trusty-mpm/changelog.d/`.

#### Acceptance criteria

- **AC1** `board-v1.yaml` parses; a test loads the embedded spec and asserts its `version` and the six standard field names. A spec with a field outside the standard set, or a Status option set that is not exactly the 9 names, fails that test.
- **AC2** Every Status option maps to an `issue-state.yaml` state or declares `label: none`; a test drops one mapping and asserts the load refuses, naming the option.
- **AC3** Each `status:*` state is reached by exactly one option; a test that maps two options to one state asserts the load refuses, naming both options.
- **AC4** Hold is present in the spec with `enabled: false`; a test asserts a spec that omits Hold is refused and a spec with Hold enabled is accepted (the switch is [#9401](https://github.com/bobmatnyc/trusty-tools/issues/9401)'s, the field is ours).
- **AC5** The `project:` block parses with owner, project number, options to drop and extra fields; a block with an unknown key is refused naming the key. trusty-tools' `issue-state.yaml` carries a block naming [#45](https://github.com/users/bobmatnyc/projects/45), owner `bobmatnyc`, and Crate and Agents as extras.
- **AC6** `git diff --name-only origin/main...HEAD` lists nothing under `content/`, and `TICKETING.md` is absent from the diff. `cargo test -p trusty-mpm --no-fail-fast`, `cargo clippy -p trusty-mpm --all-targets -- -D warnings` and `cargo fmt --check` exit 0; `tm issue standard` in this checkout prints the same output as before the change, byte for byte.
- **AC7** Closing comment on [#9400](https://github.com/bobmatnyc/trusty-tools/issues/9400) and [#9401](https://github.com/bobmatnyc/trusty-tools/issues/9401) naming the spec path, the `project:` schema and the option-to-label mapping.

#### Non-goals

Any `tm issue board` verb; GraphQL calls; Crate, Agents or any component field in the standard set; TICKETING.md or skeleton wording ([#9399](https://github.com/bobmatnyc/trusty-tools/issues/9399)); turning Hold on ([#9401](https://github.com/bobmatnyc/trusty-tools/issues/9401)); Status writes ([#9400](https://github.com/bobmatnyc/trusty-tools/issues/9400)).

#### Risk

The spec can drift from [#45](https://github.com/users/bobmatnyc/projects/45) the moment it is written if the capture is wrong; AC1 pins the field set and Phase 2's `check` run against [#45](https://github.com/users/bobmatnyc/projects/45) (Phase 3 AC1) is the end-to-end proof. An older `tm` ignores a `project:` block it does not know; AC5's unknown-key refusal is the forward half of that contract, and the backward half is a known limit recorded below.

### Phase: tm issue board init, apply, check, show and add on one Projects module

Gate: Phase 1 merged.

Ships the Projects module and the five verbs. Opens with a trial on a
throwaway project that settles the two unproven API questions, then moves the
one existing attach call onto the new module. Rung 4–5: cross-crate surface
through `gh`, a persistence contract with live GitHub, and release tooling
(`tm generate capabilities`). Decisions D2, D3, D7, D8, D9, D10 and D11 are in
the tracker.

#### Scope

- Trial, first, on a throwaway project with draft items, deleted afterwards: do option IDs and item values survive an option edit; can views be created through the API. Results go in the PR body and in the closing comment.
- New module `crates/trusty-mpm/src/bin/tm/commands/issue/board/` split into `spec`, `client`, `reconcile`, `check` and `backfill` files, each under the 500-SLOC cap. The existing `gh project item-add` in `epic/backend.rs` moves onto it.
- `init --owner <org|user> --repo <owner/repo>`: dry run by default. `--apply` refuses when a same-title or repo-linked project exists, or when `issue-state.yaml` has uncommitted changes; otherwise creates the project, replaces the default Todo/In Progress/Done options, creates the fields and views, links the repo; prints the `project:` block for the user to commit by PR; prints the manual auto-add step or generates an `actions/add-to-project` workflow.
- `apply`: idempotent reconcile, dry run by default; creates missing views by name; refuses option edits on a board with items until the trial passes; `--backfill` adds open issues (paced, resumable, paginated — [#9402](https://github.com/bobmatnyc/trusty-tools/issues/9402) later uses the same backfill module); setting Status from labels needs a further explicit flag.
- `check`: read-only. Reports drift from the spec, repo extras (not drift), the README as information only, open issues not on the board, token type and scopes from `X-OAuth-Scopes`, SSO authorisation from a 403 with `X-GitHub-SSO`. Never reads the token value.
- `show`: prints the embedded spec and its version.
- `add <N>`: adds issue N to the board in the repo's `project:` block. `tm issue standard` prints that block as the single attach target; the audit's project row checks against it. The block wins over TICKETING.md and over `agents.ticketing.default_project`, which stays only as a fallback for repos without a block; `check` reports a mismatch. `tm pr open` stops using `default_project` to add PRs to a board.
- Attach failure per D10, with the `no-project:` comment shape `epic create` already posts.
- GitHub only: refuse on a non-GitHub tracker.
- `tm generate capabilities` regenerated in the same PR, plus the content bump.
- Fake-`gh` tests for every error path; dry-run tests for `init` and `apply`.
- Live check on model-usage-monitor (duettoresearch org): `init --owner duettoresearch`, standard fields only, no Published, no On Preview; open issues backfilled with Status from labels under the explicit flag; `check` clean; its TICKETING.md set to `set_project: yes` naming the board; the owner turns on auto-add in the UI.
- Changelog fragment under `crates/trusty-mpm/changelog.d/`.

#### Acceptance criteria

- **AC1** The trial ran on a throwaway project with draft items and the project was deleted afterwards; the PR body records, for each question (option-edit survival of IDs and item values; view creation through the API), the GraphQL calls made and the observed answer. `apply` refuses option edits on a board with items unless the survival answer is yes; a fake-`gh` test asserts the refusal names the board's item count.
- **AC2** `ls crates/trusty-mpm/src/bin/tm/commands/issue/board/` lists `spec.rs`, `client.rs`, `reconcile.rs`, `check.rs` and `backfill.rs`; `./scripts/check_line_cap.sh` exits 0. `grep -rn 'item-add' crates/trusty-mpm/src/bin/tm/commands/issue/epic/` prints nothing; `epic create`'s attach goes through the module, and the four existing `create_*` attach tests still pass.
- **AC3** `init` without `--apply` prints the plan and makes no `gh` mutation call; a fake-`gh` test asserts the recorded calls are all reads. With `--apply`: a fake-`gh` test for each refusal arm — same-title project, repo-linked project, dirty `issue-state.yaml` — asserts exit non-zero and no mutation call. The success path's recorded calls create the project, replace the three default options with the 9, create Hold (off), the four dates and the views, link the repo, and print a `project:` block that parses under the Phase 1 schema.
- **AC4** `apply` run twice against the same fake board records no mutation on the second run. `--backfill` on a fake board of 250 open issues pages, paces, and resumes from a recorded cursor after a simulated failure; no issue is added twice. Status is written only when the explicit flag is present; a test without the flag asserts zero field-value mutations.
- **AC5** `check` makes no mutation call; a fake-`gh` test asserts it. It reports a missing standard field as drift, Crate and Agents as repo extras (not drift), the README as information, each open issue absent from the board, and the token type and scopes read from `X-OAuth-Scopes`. A fake 403 carrying `X-GitHub-SSO` is reported as SSO authorisation required. `grep` over the module for the token value's variable shows it is never formatted into output.
- **AC6** `add <N>` on a repo with a `project:` block attaches to that board and ignores `agents.ticketing.default_project`; a test with both set asserts the block's number is used and `check` reports the mismatch. `tm issue standard` prints the block as the attach target. The audit's project row FAILs when the issue is on a different board than the block names, and PASSes when it is on that board.
- **AC7** `tm pr open` no longer passes `default_project` to add a PR to a board; `pr_9749_a_closed_project_falls_back_to_the_default_project` is rewritten or removed with the reason in the PR body, and a test asserts no `project item-add` call on PR open.
- **AC8** One fake-`gh` test per attach-failure arm: missing-scope refusal → one `no-project:` comment, exit 0, and a second run adds no second comment; 502 → exit non-zero, no comment; SSO 403 → exit non-zero, no comment; deleted project → exit non-zero, no comment. A test feeds the open subagent `gh` token failure text from [#9750](https://github.com/bobmatnyc/trusty-tools/issues/9750) and asserts it is classified as a non-scope error, not a scope refusal.
- **AC9** `tm issue board init` on a repo whose tracker is not GitHub exits non-zero naming the tracker; a test asserts it.
- **AC10** `./scripts/check_capabilities.sh` exits 0 on the PR branch and the content bump is in the diff.
- **AC11** Live check: model-usage-monitor's board exists, created by `init --owner duettoresearch`; `check` output in the PR body shows no drift, no Published, no On Preview, every open issue on the board with Status set from its label, and the token scopes line. Its TICKETING.md names the board with `set_project: yes`.
- **AC12** `cargo test -p trusty-mpm --no-fail-fast`, `cargo clippy -p trusty-mpm --all-targets -- -D warnings` and `cargo fmt --check` exit 0; `SKIP_UI_BUILD=1 cargo check --workspace` exits 0.
- **AC13** Closing comment on [#9397](https://github.com/bobmatnyc/trusty-tools/issues/9397), [#9400](https://github.com/bobmatnyc/trusty-tools/issues/9400) and [#9402](https://github.com/bobmatnyc/trusty-tools/issues/9402) with the trial results and the module path and API.

#### Non-goals

Status writes on transition ([#9400](https://github.com/bobmatnyc/trusty-tools/issues/9400)); turning Hold on ([#9401](https://github.com/bobmatnyc/trusty-tools/issues/9401)); label-to-Status authority migration ([#9402](https://github.com/bobmatnyc/trusty-tools/issues/9402)); enabling a built-in auto-add workflow through the API (GitHub exposes read/delete only); PRs as board items; GitHub Enterprise Server; copying anything from [#45](https://github.com/users/bobmatnyc/projects/45); any change to [#45](https://github.com/users/bobmatnyc/projects/45); TICKETING.md wording ([#9399](https://github.com/bobmatnyc/trusty-tools/issues/9399)).

#### Risk

The trial may answer no to either question. A no on option-edit survival leaves `apply` refusing option edits on boards with items, which is the designed fallback. A no on view creation triggers the open decision recorded below; the phase ships without views created by `init` and the decision is made after the trial, not inside this phase. User-owned boards need a classic PAT and `GITHUB_TOKEN` cannot write Projects; `check`'s scope report is the diagnosis path, and AC8's classifier test keeps the [#9750](https://github.com/bobmatnyc/trusty-tools/issues/9750) token failure from being misread as a scope refusal.

### Phase: ticketing agent and Architect checklist call tm issue board

Gate: Phase 2 merged.

Wires the content: the ticketing agent calls `tm issue board add <N>` after
each issue it files, and the Architect provisioning checklist runs
`tm issue board check`. Rung 1–3: content only, verified by the content
checks and one live `check` run against [#45](https://github.com/users/bobmatnyc/projects/45). Decisions D8 and D12 are in the
tracker.

#### Scope

- `content/agents/ticketing.md`: after filing an issue, the agent runs `tm issue board add <N>`; the raw `gh issue create --project` / `gh issue edit --add-project` instruction is replaced by that call.
- `content/instructions/architect/skills/architect-project-provision.md`: the provisioning checklist runs `tm issue board check` and reports its output.
- `content/manifest.toml` bump and `content/changelog.d/` fragment, as the content gate requires.

#### Acceptance criteria

- **AC1** `tm issue board check` run in this checkout against [#45](https://github.com/users/bobmatnyc/projects/45) with trusty-tools' `project:` block reports no drift, Crate and Agents as repo extras; the output is in the PR body.
- **AC2** `grep -c 'tm issue board add' content/agents/ticketing.md` prints at least 1 and the line sits in the filing procedure after `gh issue create`; `grep -c -- '--add-project' content/agents/ticketing.md` prints 0.
- **AC3** `grep -c 'tm issue board check' content/instructions/architect/skills/architect-project-provision.md` prints at least 1.
- **AC4** `python3 scripts/check_content.py tree` and `python3 scripts/check_content.py bump` exit 0; `./scripts/check_capabilities.sh` exits 0.
- **AC5** `git diff --name-only origin/main...HEAD -- TICKETING.md` prints nothing.
- **AC6** Closing comment on [#9398](https://github.com/bobmatnyc/trusty-tools/issues/9398), asking its rewrite to keep the `board add` line, and on [#9399](https://github.com/bobmatnyc/trusty-tools/issues/9399).

#### Non-goals

TICKETING.md or skeleton wording ([#9399](https://github.com/bobmatnyc/trusty-tools/issues/9399)); the ticketing agent rewrite tracked by [#9398](https://github.com/bobmatnyc/trusty-tools/issues/9398) beyond the one `board add` line; any crate change; the daemon bug report and the trusty-agents ticketing client as issue creators.

#### Risk

The ticketing agent's instruction can be read on a host whose `tm` predates Phase 2; the agent then meets an unknown-subcommand error. The content bump and the `tm generate capabilities` regeneration in Phase 2 keep the deployed skill and binary in step, and until this phase lands agent-filed issues reach the board only through current instructions and auto-add.

### Deferred

<!-- deferred:start -->
| Item | Why deferred | Where it went |
|------|--------------|---------------|
| Views fallback when the Phase 2 trial shows views cannot be created through the API — (a) a manual checklist in the board README with `check` reporting missing views as information, or (b) a template project owned by trusty-tools (never [#45](https://github.com/users/bobmatnyc/projects/45)) that `init` copies | Decide after the trial, not before | this epic, after Phase 2's trial |
| Enabling a built-in auto-add workflow from `tm` | GitHub exposes built-in workflows as read/delete only; the UI or the `actions/add-to-project` Action with a stored token is the only path | unscheduled |
| Status writes on transition | Owned by [#9400](https://github.com/bobmatnyc/trusty-tools/issues/9400), blocked-by this epic | [#9400](https://github.com/bobmatnyc/trusty-tools/issues/9400) |
| Turning Hold on | Owned by [#9401](https://github.com/bobmatnyc/trusty-tools/issues/9401); the spec declares the field off | [#9401](https://github.com/bobmatnyc/trusty-tools/issues/9401) |
| Label-to-Status authority migration | Owned by [#9402](https://github.com/bobmatnyc/trusty-tools/issues/9402); option remapping moved here, the rest stays | [#9402](https://github.com/bobmatnyc/trusty-tools/issues/9402) |
| GitHub Enterprise Server | Out of scope by owner ruling | unscheduled |
| The daemon bug report and the trusty-agents ticketing client as issue creators | Out of scope by owner ruling | unscheduled |
<!-- deferred:end -->

## Open decision

Recorded, not decided. If the Phase 2 trial shows views cannot be created
through the API, one of two fallbacks is chosen after the trial:

- (a) a manual checklist in the board README; `check` reports missing views as
  information, not drift;
- (b) a template project owned by trusty-tools — never [#45](https://github.com/users/bobmatnyc/projects/45) — that `init`
  copies.

## Known limits

- Auto-add needs the UI or the `actions/add-to-project` Action with a stored
  PAT or App token.
- User-owned boards need a classic token.
- An older `tm` silently ignores a `project:` block.
- Until Phase 3 lands, agent-filed issues reach the board only through current
  instructions and auto-add.

## Maintenance

The phases block is regenerated from child issue state — never patched by
hand. The deferred block is amended deliberately, only when scope leaves the
plan or a deferred item lands. The follow-ups block is appended as findings
surface; at most two of them per phase become standalone issues, and only at
severity HIGH or above. The sections above the markers are authored once.

Comments on the tracker are limited to changes of plan: a decision made, a
phase re-scoped, a risk realized, an item deferred, a gate lifted. Progress
belongs in the phases block; evidence belongs in the phase issue or its PR.

Issue numbers are not written back here. The `TBD` cells in the phases block
above are this document's own copy; the tracker's block supersedes them once
the epic is filed.

## Filing

Metadata `tm issue epic create` does not read from the plan. Applied by the
filing session.

### Tracker

- Title at creation: `[EPIC] Every repo adopting trusty-mpm gets a standard GitHub Projects board, created from nothing, and tm adds workflow issues to it`; retitled `[EPIC <N>] …` after read-back.
- Labels: `epic`, `trusty-mpm`, `area:session-lifecycle`, `ws/<session>`.
- Milestone: the open trusty-mpm milestone at filing. Project [#45](https://github.com/users/bobmatnyc/projects/45), Status Backlog.
- Blocked-by link: [#9400](https://github.com/bobmatnyc/trusty-tools/issues/9400) is blocked-by `<N>`.
- Coordination comments in the same dispatch: [#9397](https://github.com/bobmatnyc/trusty-tools/issues/9397) (standalone epic, not a phase); [#9400](https://github.com/bobmatnyc/trusty-tools/issues/9400) (blocked-by `<N>`; uses this epic's module, `project:` schema and mapping); [#9401](https://github.com/bobmatnyc/trusty-tools/issues/9401) (Hold declared off in the spec; correction — Hold already exists on [#45](https://github.com/users/bobmatnyc/projects/45)); [#9402](https://github.com/bobmatnyc/trusty-tools/issues/9402) (option remapping moves to `tm issue board apply`; backfill module shared).
- Close rule: all three phases closed; closing comment maps O1–O4 to evidence.

### Phase 1

- Title: `[EPIC_<N> PHASE_1] board-v1.yaml spec and the issue-state.yaml project block`.
- Labels: `enhancement`, `trusty-mpm`, `area:session-lifecycle`, `ws/<session>`. Milestone: parent's. Project [#45](https://github.com/users/bobmatnyc/projects/45), Backlog. Parent `<N>`.
- Rung 3. Close rule: Merged.
- PR title: `feat(trusty-mpm): [EPIC_<N> PHASE_1] embed board-v1.yaml and the issue-state project block`.

### Phase 2

- Title: `[EPIC_<N> PHASE_2] tm issue board init, apply, check, show and add on one Projects module`.
- Labels: `enhancement`, `trusty-mpm`, `area:session-lifecycle`, `ws/<session>`. Milestone: parent's. Project [#45](https://github.com/users/bobmatnyc/projects/45), Backlog. Parent `<N>`; blocked-by `<P1>`.
- Rung 4–5. Close rule: On Prod.
- PR title: `feat(trusty-mpm): [EPIC_<N> PHASE_2] tm issue board init/apply/check/show/add`.

### Phase 3

- Title: `[EPIC_<N> PHASE_3] ticketing agent and Architect checklist call tm issue board`.
- Labels: `documentation`, `area:agent-prompts`, `ws/<session>`. Milestone: parent's. Project [#45](https://github.com/users/bobmatnyc/projects/45), Backlog. Parent `<N>`; blocked-by `<P2>`.
- Rung 1–3. Close rule: Merged.
- PR title: `docs(content): [EPIC_<N> PHASE_3] ticketing agent runs tm issue board add; Architect checklist runs check`.
- Comment in the same dispatch: `no-component-label: no crate owns content/agents/ or content/instructions/`.

### Sequence

1. Commit this document to `origin/main` through a PR; record the permalink SHA.
2. Create the `ws/<session>` label if it does not exist; run `tm issue seed-labels`.
3. `tm issue epic create --from docs/research/project-board/plan.md --milestone "<milestone>" --component trusty-mpm --project 45 --session <session> --phase-type enhancement`.
4. Post-create edits (the CLI sets neither assignee nor Project Status): Project [#45](https://github.com/users/bobmatnyc/projects/45) Status Backlog on all four; Phase 3 type `documentation` and its `trusty-mpm` label removed; `area:*` on all four; blocked-by P1 → P2, P2 → P3, and `<N>` → [#9400](https://github.com/bobmatnyc/trusty-tools/issues/9400).
5. Coordination comments on [#9397](https://github.com/bobmatnyc/trusty-tools/issues/9397), [#9400](https://github.com/bobmatnyc/trusty-tools/issues/9400), [#9401](https://github.com/bobmatnyc/trusty-tools/issues/9401) and [#9402](https://github.com/bobmatnyc/trusty-tools/issues/9402); `no-component-label` comment on Phase 3.
6. `tm issue audit` on all four; `tm issue epic sync <N>`.
