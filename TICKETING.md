# TICKETING.md — trusty-tools ticketing standard

Standard of record for every issue in this repository. The `ticketing` agent
reads this before any create, label, comment, or transition, in any tracker. It
overrides the `tm-ticketing` skill defaults and the per-machine
`agents.ticketing` block in `~/.trusty-tools/trusty-mpm/config.yaml`. A value
this file does not state falls through to those, in that order.

Generated 2026-09-22 from the live repository state (`gh label list`, the open
milestones, the owner's projects) plus `CLAUDE.md` and `issue-state.yaml`. Edit
any value; nothing regenerates or reformats this file.

## Tracker

- Tracker: `gh` (GitHub Issues), repository `bobmatnyc/trusty-tools`.
- Every issue operation routes through the `ticketing` agent — create, edit,
  label, assign, milestone, comment, reopen, close. A PM or another agent
  running `gh issue` directly is a routing error.
- Pull requests are `version-control`'s, including the PR title and body.

## Labels

**Type — exactly one:** `bug`, `enhancement`, `refactor`, `chore`,
`documentation`, `epic`.

**Component — one or more**, chosen from the file paths the finding cites, never
from the harness the session runs under. **This repository's stack unit is the
Cargo crate** (`crates/<name>/`), so a component label names a crate or one of
the subsystem labels below. Resolve abbreviations against `CLAUDE.md`'s
"Abbreviations & Aliases" table first.

Crate labels: `trusty-agents`, `trusty-agents-common`, `trusty-agents-local`,
`trusty-agents-ui`, `trusty-analyze`, `trusty-audit`, `trusty-audit-ui`,
`trusty-bm25-daemon`, `trusty-channels`, `trusty-code`, `trusty-code-gui`,
`trusty-code-tui`, `trusty-common`, `trusty-console`, `trusty-controller`,
`trusty-crate-contracts`, `trusty-cto-db`, `trusty-embedderd`,
`trusty-embedderd-py`, `trusty-gworkspace`, `trusty-installer`, `trusty-kb`,
`trusty-mcp`, `trusty-memory`, `trusty-mpm`, `trusty-mpm-gui`, `trusty-review`,
`trusty-progress`, `trusty-publish-guard`, `trusty-search`, `trusty-sld-lint`,
`tc-services`, `tga`, `cto-assistant`.

Subsystem labels for paths no crate owns: `ci`, `daemon`, `deps`, `dx`,
`launchd`, `mcp`, `monitor`, `ops`, `performance`, `spec`, `test`, `ui`.

**When no component label fits the path, apply none** and post
`no-component-label: <reason>` as a comment in the same dispatch — the prefix is
parsed literally, so keep it byte-for-byte, and write the reason as "no crate
owns `<path>`" since crates are this repository's unit. That comment is what
makes the absence legitimate; `tm issue audit` reads it and prints
`component label  SKIP  <reason>` instead of FAIL.

**Priority — optional:** `P0` (drop everything), `P1` (high), `P2` (medium),
`P3` (low). Applied **only** when the issue text itself asserts severity — an
explicit "P1", or language like data loss, unrecoverable, silent corruption. A
guessed priority is noise someone else re-triages.

**Workstream — one:** `ws/<session-name>`, the filing session's. Always a
label, never a milestone.

**Other labels in use**, applied when they describe the issue: `blocked`,
`breaking-change`, `do-not-merge`, `duplicate`, `regression`, `release`,
`release-cleanup`, `security`, `self-improvement`, `tech-debt`, `wont-do`.

**Not part of this standard, and never applied by the agent:** the
`unicorn:*` family, `blast:*`, `approval:*`, and `T2`–`T4`. These belong to the
Unicorn Factory and PR-tier tooling, which set them themselves.

**Creating a missing label is allowed** after `gh label list -R
bobmatnyc/trusty-tools --limit 200` confirms it is absent — report what you
created. Run `tm issue seed-labels` first; it is idempotent and creates the four
`status:*` labels, `trusty-mpm`, and `ws/<session>`.

## Milestones

Live titles, as of generation:

- Crate backlogs: `Backlog · agents`, `Backlog · analyze/review`,
  `Backlog · audit`, `Backlog · code`, `Backlog · console`,
  `Backlog · embedderd`, `Backlog · installer`, `Backlog · mcp`,
  `Backlog · memory (triaged)`, `Backlog · mpm/core`, `Backlog · search`,
  `Backlog · tc-services`, `Backlog · tga`.
- Version milestones: `1.7.1`, `1.7.2`, `trusty-mpm 2.0.0`.
- Epic milestones: `Issue management` (#94) — issue-management work and its
  follow-ups; `Instructional content` (#95).
- Themed: `Advisory exception review — 2026-09`,
  `Fail-open & silent-success fixes · mpm/core`,
  `Session, worktree & daemon lifecycle · mpm/core`, `trusty agents mvp`,
  `trusty-agents 1.0 — assistant platform`,
  `trusty-code R1 · Reliable independent core`,
  `trusty-code R2 · Shared instructions, agents & skills`,
  `trusty-code R3 · MCP & channel interoperability`,
  `trusty-code v0.7.0 · Claude Code TUI parity + PM-delegated coding tasks`,
  `trusty-mpm dashboard — machine-wide event visualization (DOC-73)`.

Read the live list from `tm issue standard` rather than this snapshot before
filing; never hand-type a title neither source printed.

**Selection rule, in order:**

1. The parent's milestone, when the issue is a sub-issue.
2. Otherwise the owning crate's `Backlog · <crate>` milestone.
3. Otherwise the one `tm issue standard` names for the repository.

**Unset milestone:** file with none and post `no-milestone: <reason>` as a
comment in the same dispatch. An issue holds many labels and exactly one
milestone, so a workstream parked there evicts the real slot.

## Projects

Live projects, by number, owner `bobmatnyc`:

| # | Project | # | Project |
|---|---|---|---|
| 22 | trusty-memory | 29 | trusty-console |
| 23 | trusty-search | 38 | cross-crate + trusty-common |
| 24 | trusty-analyze + trusty-review | 39 | tc-services |
| 25 | trusty-mpm | 40 | tga |
| 26 | trusty-code | 41 | trusty-audit |
| 27 | trusty-agents | 42 | trusty-embedderd |
| 28 | trusty-installer | 43 | trusty-mcp |
| | | 44 | trusty-tools · CI & infra |

**Selection rule:** the project of the owning crate. A change spanning crates,
or one in `trusty-common`, goes to #38. A CI, workflow, or `scripts/` issue goes
to #44.

**Attach by number and owner**, never by title (#7952 — the title form exits 0
and attaches nothing):

```bash
gh project item-add 25 --owner bobmatnyc --url https://github.com/bobmatnyc/trusty-tools/issues/N
```

## Relationships

- Parent/child: GitHub's native sub-issue API — `gh issue create --parent <n>`,
  `gh issue edit <n> --parent <p>`.
- Sequencing: GitHub's native blocked-by dependency API.
- PR → issue keyword: `Refs #N`, **never** `Closes #N` (fixed; see below).
- "Companion", "related", and a prose task list are not relationships. Use a
  sub-issue, a blocked-by, or nothing.
- Every relationship the brief names is set on the call that files the issue and
  reported. One left for a follow-up pass does not exist.

## Lifecycle

States, in order: `open` → `status:in-progress` → `status:coded` →
`status:merged` → `status:tested` → `closed`. The four `status:*` labels are
mutually exclusive.

| Label | Meaning |
|---|---|
| `status:in-progress` | A session/agent has claimed it and is actively working it |
| `status:coded` | Implementation pushed on a branch; PR not yet merged |
| `status:merged` | PR merged to main; rung 4–6 fixes await live verification |
| `status:tested` | Verified live (installed binary / real run); eligible to close |

- State model file: [`issue-state.yaml`](issue-state.yaml) at the repo root —
  the authority on which edges exist and which require a note.
- Transition command: `tm issue transition N <state>`. Never a hand-typed label
  swap; the tool issues the add and the remove as one `gh issue edit`. On a host
  with no `tm`, and only there: `gh issue edit N --add-label <new>
  --remove-label <old>`, both flags in one call.
- `tm issue states` lists the edges, `tm issue current N` reads a state back,
  `tm issue repair N` drops a stale second `status:` label.

**Close bar, by test-ladder rung** (`CLAUDE.md`, "Rust Test Ladder"):

- Rung 1–3 — closes at merge, skipping `status:merged` and `status:tested`:
  `tm issue transition N closed --note "PR #M squash <sha>"`.
- Rung 4–6 — CLI, daemon, and hook fixes needing live proof — close only from
  `status:tested`, with the live evidence in the note.
- Docs-only — closes from `status:merged` on merged-text evidence: the squash
  SHA plus confirmation the text is present on `origin/main` at the cited
  file/symbol.
- A merged fix that fails live verification stays open, at `status:merged`,
  returning to `status:coded` only when a follow-up fix PR is open.

## Comment conventions

| Event | Comment posted | It carries |
|---|---|---|
| Claim at dispatch | yes | `Claimed by session <name>, <YYYY-MM-DD> — <what is in flight>.` |
| `status:coded` | yes | The PR link |
| `status:merged` | yes | The squash SHA, and the post-merge CI run on main |
| `status:tested` | yes | What was run against the installed binary and what it printed |
| Close | yes, as `--note` | `PR #M squash <sha>` for rung 1–3; the live evidence for rung 4–6 |
| Blocked | yes | The blocker, its impact, and the unblock criteria |
| No component label | yes | `no-component-label: <reason>` |
| No milestone | yes | `no-milestone: <reason>` |
| Review verdict | only when it changes the issue's state | The verdict and the resulting state |
| Polling / progress | no | — |

Every issue body and every comment ends with the attribution line below.

## Behaviour settings

| Setting | Value |
|---|---|
| `comment_on` | claim, `status:coded`, `status:merged`, `status:tested`, close, blocked, `no-component-label`, `no-milestone` |
| `transitions` | automatic and event-driven — the agent that observed the event owes the label pass in that dispatch; nothing sweeps later |
| `assign` | `--assignee @me` on creation |
| `set_milestone` | yes, on the creating call |
| `set_project` | yes, on the creating call |
| `create_missing_labels` | yes, after `gh label list` confirms absence; `tm issue seed-labels` first |
| `dedupe_strictness` | strict — search open AND closed on all four keys below before every filing; `COMMENT` beats `NEW` whenever an open issue covers the same defect |
| `comment_verbosity` | pointer-first — links and the decisive line; evidence inline only where the reader cannot act without it |
| `rollup_issue` | [#8021](https://github.com/bobmatnyc/trusty-tools/issues/8021) |
| `pr_issue_link` | `Refs #N` (fixed) |
| `status_on_creation` | plain `open` — filing is not a dispatch; `status:in-progress` waits for a brief that says work starts now |
| `audit_after_filing` | yes — run `tm issue audit <N>` and paste its output into the report |
| `epics.title_format` | `[EPIC <epic#>] <outcome>` for the tracker (created `[EPIC]`, renamed once the number is known); phases `[EPIC_<epic#> PHASE_<n>] <what>` |
| `epics.tracker_autoupdate` | `true` — `<!-- phases:start -->` regenerated wholesale, `<!-- deferred:start -->` amended, nothing outside the markers touched |
| `epics.update_triggers` | phase opens, phase closes, phase blocks/unblocks, item deferred or landed |
| `research_docs_path` | `docs/research/<effort>/` — trackers link to the doc; no issue body carries findings |
| `component_unit` | Cargo crate (`crates/<name>/`) |
| `followups.budget_per_phase` | `2` |
| `followups.severity_floor_for_standalone` | `HIGH` |
| `followups.tracker` | the epic's Follow-ups checklist; `rollup:#8021` when there is no epic |
| `followups.due_within_days` | `14` |
| `staleness.stale_after_days` | `30` |
| `staleness.close_stale_after_days` | `60` |
| `staleness.exempt_labels` | `keep`, `paused`, `blocked` |
| `staleness.exempt_when_milestoned` | `true` |
| `staleness.decision_request` | digest — one comment per epic tracker, never a question per issue |
| `coverage` | every open issue belongs to an epic or to a `Backlog · <area>` milestone |

**Claim reclaim.** Another session takes a claimed issue only when the claim is
provably stale: the named session is gone AND nothing referencing the issue —
branch push, PR, comment — has moved since the claim. Either alone is not
enough.

## Epics — trackers and phase issues

The pattern is [`docs/reference/tracker-phases-pattern.md`](docs/reference/tracker-phases-pattern.md),
committed verbatim. Read it before creating a tracker. Use it only when the work
has a **gate** between stages — one stage verified, soaked, or deployed before
the next starts. No gate means no tracker; one issue with a task list costs less.

- Tracker title: `[EPIC <epic#>] <the outcome, in plain words>`. Label `epic`.
- Phase title: `[EPIC_<epic#> PHASE_<n>] <what this phase does>`, where
  `<epic#>` is the tracker's own issue number.
- Create the tracker FIRST titled `[EPIC] <outcome>`, read its number back, and
  rename the title in place before filing phase issues — they cannot go in the
  same batch. Link them as **native sub-issues**, never a markdown task list.
- Tracker body, three zones and three maintenance rules: everything above the
  markers is authored once; the `<!-- phases:start -->` block is regenerated
  wholesale from child-issue state; the `<!-- deferred:start -->` block is
  amended deliberately. **Nothing outside the markers is ever touched.**
- The `Gate` column is what justifies using the pattern at all. An empty
  Ordering section means the work did not need it.
- Four update triggers, and only these: a phase opens, a phase closes, a phase
  blocks or unblocks, an item is deferred or a deferred item lands. Not on PR
  open, merge, commit, or review.
- Phase numbers are assigned once, never renumbered, never reused. A phase
  inserted later between 2 and 3 is `PHASE_6`; the table says where it runs.
- The tracker closes when every phase is closed and each outcome is verified,
  with a closing comment mapping O1..On to evidence, one line each.

Live trackers in this repository: [#8380](https://github.com/bobmatnyc/trusty-tools/issues/8380) — milestone
"Issue management" (#94), sub-issues [#8376](https://github.com/bobmatnyc/trusty-tools/issues/8376) and
[#8379](https://github.com/bobmatnyc/trusty-tools/issues/8379); and [#8378](https://github.com/bobmatnyc/trusty-tools/issues/8378) — milestone "Instructional
content" (#95). Both predate this naming and keep their existing titles; new
trackers use the form above.

## Research

- `research_docs_path`: `docs/research/<effort>/`.
- An epic is created from prior research. **How** the research happens stays
  flexible; **what** it produces is a committed document at that path, and the
  tracker links to it.
- No issue body carries findings, evidence dumps, or analysis. That is what
  makes "Epic #8380, work on phase 5" a complete instruction: the context is in
  the repository, readable by any contributor, and it outlives the tracker.

## Follow-ups

This repository carries 400+ open issues; it got there through follow-ups, one
reasonable filing at a time. The policy is **critical or independently
schedulable, always linked, always dated, always budgeted** — not "no follow-up
issues", which loses the critical ones, and not "critical only" without
linkage, which loses the epic.

A follow-up becomes a **standalone issue** only when it is `HIGH` or above AND
within the budget of **2 per phase issue**. When it is, it carries all four:

1. Its trigger — `Refs #<phase issue>` and the PR that surfaced it.
2. A native sub-issue link to the epic.
3. A `P0`–`P3` label.
4. A milestone, or a `due-by: <YYYY-MM-DD>` line within 14 days. An
   issue-management follow-up takes the `Issue management` milestone.

Everything else — below `HIGH`, or past the budget — is a checklist line on the
epic's Follow-ups tracker, or on rollup
[#8021](https://github.com/bobmatnyc/trusty-tools/issues/8021) when the work has
no epic. Report the budget spent and what was routed to a tracker instead.

## Staleness

At **30 days** with no activity and no milestone, the agent labels `stale` and
posts ONE triage comment naming its recommended disposition with evidence:

| Disposition | Evidence it carries |
|---|---|
| CLOSE | The code path is gone, or a merged PR superseded it — cite the PR |
| SUPERSEDE | The newer issue, linked; this one closes against it |
| KEEP | Why it still matters, and a re-dating |

At **60 days** it closes with a note, unless the issue is pinned to a milestone
or carries `keep`, `paused`, or `blocked`.

🔴 **A human decision is requested as a digest, never per issue.** Group the
recommendations per epic and post them as one comment on that epic's tracker.
The sweep is `tm-issues-prune`'s Prune phase; this section is the policy it
applies.

## Coverage and metrics

- Every open issue belongs to an epic or to a `Backlog · <area>` milestone. One
  with neither is a standard violation the audit reports.
- The project board is filtered by epic; the epic tracker is the single view a
  human reads.
- `metrics:` — open count per epic, follow-ups per phase issue, stale count,
  median age of open follow-ups. These are the contract for the `tm issue audit`
  / `issue_audit_recent` integration, not something implemented today.

## Dedupe and promotion

**Search keys**, all four, open and closed, before every filing — the rationale
is in [`docs/reference/issue-search-keys.md`](docs/reference/issue-search-keys.md):

1. Test name — `execute_doctor_against_test_daemon`.
2. Panic or error text — the literal message, including a `thiserror` Display
   string.
3. Affected symbol — `WatcherManager::reconcile`.
4. Crate — `-p trusty-search`, abbreviations expanded first.

A fifth pool: the 2026-08-14 "1.3.8 backlog reset" closed 277 issues on age, not
on verification. A hit there is prior art worth reading.

**Dispositions**, exactly one per finding, reported by name: `COMMENT`,
`REOPEN`, `NEW REGRESSION`, `NO TICKET`.

**What never becomes its own issue here:**

- A `code-critic` / `code-analyzer` / trusty-review finding below HIGH.
- A self-improvement or post-mortem finding — the `self-improvement` label,
  `tm-postmortem` output, `report_bug` / `preview_bug_report`, or an agent's
  "Improvement recommendations" block.
- A PM or agent "Prompt feedback" addendum item.

Each is fixed in the surfacing PR, dropped, or logged as a dated comment on the
rollup issue [#8021](https://github.com/bobmatnyc/trusty-tools/issues/8021),
deduplicated against earlier comments. HIGH+ or independently schedulable work
may still be filed — search first.

## Title and body

- **Title:** `<type>(<crate>): <what is wrong or wanted>`, under ~70 characters.
  `fix(trusty-search): watcher misses renames on external volumes`, not "Bug in
  system".
- **Body:** the defect or outcome, the decisive evidence, and one to four
  observable closure conditions. Nothing else. No structured headings. Point at
  a linked issue, spec, ADR, or PR rather than restating it. Cite file and
  symbol — `agent_source.rs::autodeploy_agents` — never line numbers. Past a
  short screen it is over-written.
- A `bug` body states its confidence state (Observed / Reproduced / Inferred /
  Speculative). A feature, task, epic, or spec issue carries no confidence line.
- Three shapes may run longer, for evidence and never for narrative: an `epic`
  (child checklist and the scope boundary between children), a security issue
  (impact, affected versions, disclosure state), and a research or audit issue
  (its evidence inventory).
- Every reference renders as a clickable markdown link, never a bare number.

## Attribution

Every issue body and every issue comment ends with exactly one line, no preamble
around it:

```
🤖🤖🤖 Generated with trusty-mpm — https://github.com/bobmatnyc/trusty-tools
```

Commit and PR footers come from the `attribution` key tm writes into the
provisioned Claude Code settings — not from this file.

## Fixed regardless of this file

- **`Refs #N`, never `Closes #N`.** A merge must not auto-close an issue nobody
  has verified live. The one-off `Closes` is the deliberate `tm pr open
  --closes` flag, chosen per PR.
- **`trusty-mpm` is a component label, never a lifecycle one**, and never a
  marker for which session surfaced a finding. It applies only when the code at
  fault sits under `crates/trusty-mpm/` or the tm CLI's own release tooling.
