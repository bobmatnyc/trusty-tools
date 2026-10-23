---
name: tm-ticketing
description: The single authority on issues — whether one should exist, deduplication disposition, title/body style, labels, milestones, lifecycle comments, attribution, and the project-root TICKETING.md that overrides these defaults
user-invocable: true
metadata:
  version: "2.2.0"
category: pm-workflow
tags: [tickets, issues, promotion-gate, deduplication, labels, pm-required]
effort: medium
---

# tm-ticketing — Issue Policy

<!-- #5202: this skill owns the ISSUE. It owns no git operation and no PR
     mutation, including PR title and body. The delivery chain that wraps it is
     `tm-workflow`. -->

The PM never calls ticketing tools directly — always delegate to the `ticketing`
agent (P6 / CB#6). This skill is what the PM reads before deciding to file,
comment on, or close anything.

## Scope

Yours: whether an issue should exist, search and dedup, issue title and body,
labels, assignee, milestone, comments, state, parent/child links.

Not yours: any git operation, and **any PR mutation — including the PR title and
body**. Those are the `version-control` agent's, delegated by the PM. Ticketing
supplies the canonical issue context that goes *into* the PR body; version
control writes it. The full boundary and the handoff sequence are stated once, in
`tm-workflow`.

For session-local task tracking that is not a formal ticket, use
`mcp__trusty-memory__task_add` / `task_list` / `task_complete` directly. These
are in-session TODOs, not a ticketing system, and are not a forbidden MCP family
under CB#6.

## Backends

The `ticketing` agent is granted the full tool set, so `gh` is available to it
whichever backend a project uses. Its priority order:

1. `mcp__mcp-ticketer__*` MCP tools, when configured.
2. GitHub Issues via `gh issue …` (or `mcp__github__*`) when the project's
   tracker is GitHub — the common case.
3. `aitrackdown` CLI when neither is available.

Route **every** Issue API operation to `ticketing`, whichever backend applies.

## The Standard of Record — `TICKETING.md`

🔴 **Everything else in this skill is a DEFAULT.** A project states its own
ticketing standard in a `TICKETING.md` at its root, and that file wins (owner
ruling 2026-09-22). The `ticketing` agent reads it before any create, label,
comment, or transition, in **every** tracker — GitHub, mcp-ticketer,
aitrackdown, or one added later — and generates it from the skeleton in
`references/ticketing-md-skeleton.md` when it is absent. It governs BEHAVIOUR as
well as taxonomy.

**Resolution order**, highest first:

1. **Repo-root `TICKETING.md`**, discovered by walking upward from the session's
   project directory and stopping at the git toplevel — the same walk
   `issue-state.yaml` gets (`commands/issue/config.rs::discover_upward`). A
   sibling repository's file is never this repository's standard.
2. **The `agents.ticketing` block** in `~/.trusty-tools/trusty-mpm/config.yaml`,
   which `tm issue standard` prints. It is per-machine, so it never outranks a
   file the project committed and reviewed.
3. **This skill's defaults** — this file's rules, and the reference files
   "Where Each Task Lives" names.

🔴 **Two things no tier changes**, and `tm issue standard` prints both:

- **The PR issue-link keyword stays `Refs #N`.** A merge must not auto-close an
  issue nobody has verified live. The one-off `Closes` is the deliberate
  `tm pr open --closes` flag, chosen per PR, never a project-wide setting.
- **`trusty-mpm` stays a component label, never a lifecycle one.** Which
  session, harness, or agent surfaced a finding is not a labeling axis under any
  name, so no file may promote it to one.

A `TICKETING.md` that sets either is honoured on everything else and refused on
those two, with the field named — the same refusal the config block gets.

## Where Each Task Lives

This file holds the rules every ticketing task needs. Everything else sits in
one file per task under `references/`, beside this file. Read the file your task
needs, and only that file. A pointer elsewhere that names a section of
`tm-ticketing` resolves here: find the section name in the middle column.

| Task | Sections | File |
|---|---|---|
| Apply the defaults a silent `TICKETING.md` leaves in force | "Default standard — the taxonomy", "Default behaviour" | [references/default-standard.md](references/default-standard.md) |
| Generate a missing `TICKETING.md` | "Generating `TICKETING.md`" — the skeleton | [references/ticketing-md-skeleton.md](references/ticketing-md-skeleton.md) |
| Create an epic tracker or phase issue, or link its research | "Trackers and phase issues", "Research goes in committed docs, never in issues" | [references/epics-and-research.md](references/epics-and-research.md) |
| Decide whether a follow-up becomes an issue; run a staleness sweep; check coverage | "Follow-ups — the sprawl control", "Staleness", "Scale rules" | [references/followups-staleness-scale.md](references/followups-staleness-scale.md) |
| Decide whether a finding becomes an issue at all | "Ticket-Promotion Gate": "Search first, then choose a disposition", promotion criteria, confidence state, sizing | [references/promotion-gate.md](references/promotion-gate.md) |
| Write an issue title and body | "What a Ticket Says", "Symptom Titles and Area Labels" | [references/ticket-content.md](references/ticket-content.md) |
| Apply type, component and priority labels; waive a component label; seed labels | "Labels" | [references/labels.md](references/labels.md) |
| Choose and set the milestone and the project; check a filing | "Choosing the Milestone and the Project (issues)", "Projects", "#7952" | [references/milestones-and-projects.md](references/milestones-and-projects.md) |
| Set or read parent/child and blocked-by links; close a parent | "Relationships (native, never prose)", "On closing a parent", "Epics and phases" | [references/relationships.md](references/relationships.md) |
| Claim, transition, comment on or close an issue; sign it | "Lifecycle — open → in-progress → coded → merged → tested → closed", "The close bar", "Attribution on Issues and Comments" | [references/lifecycle.md](references/lifecycle.md) |
| Run `/tm-ticket`; route research docs with ticket context | "`/tm-ticket` Subcommands", "Documentation Routing With Ticket Context" | [references/commands-and-routing.md](references/commands-and-routing.md) |
| Open these files from an agent with no `Skill` tool | the Read path | [references/README.md](references/README.md) |

## Ask Before Creating

If the user references a ticket or issue and no matching one is found, ticketing
MUST NOT auto-create. Ask: "I didn't find an existing issue for [topic]. Create
one, or did you mean a different one?" Auto-create only on an explicit "create a
ticket/issue for X."

## Clickable References — the Link Shapes

Moved out of the instruction package by #7423, which keeps the rule itself:
every reference to an issue, PR, ticket, or commit renders as a clickable
markdown link, never a bare number, in every artifact you author.

- Issues and PRs: `[#4318](https://github.com/<owner>/<repo>/issues/4318)`.
  GitHub resolves the `/issues/` form to a PR, so one shape covers both.
- Commits: `[d027ef1](https://github.com/<owner>/<repo>/commit/d027ef1)`. A bare
  short SHA is acceptable only inside a table of many.
- Tickets in another tracker: link to that tracker's issue URL.

## Every issue: milestone, relationships, state

<!-- rule:tm-ticketing/every-issue-milestone-relationships-state -->
🔴 **Every issue you file or touch carries three things: a milestone, its
relationships, and its current state.** This section is the one statement of
the rule (owner rule 2026-10-19, #10045). Other skills and agents point here
and never restate it.

**Milestone.** Set one on every issue you file, and on every issue you touch
that has none. Pick it by the rule in "Choosing the Milestone and the Project
(issues)".

<!-- rule:tm-ticketing/propose-a-milestone-set -->
**No usable milestone set.** When the repository has no open milestone that
rule can pick, propose a set to the PM: each milestone's name and the scope it
holds. Never leave the milestone unset without a proposal: file the issue, post
`no-milestone: proposed <names> to the PM` on it, and put the proposal in your
report. Create the milestones only when the PM says to.

**Relationships.** Record each one that exists, natively, as "Relationships
(native, never prose)" describes:

- parent and sub-issues;
- blocked-by and blocking;
- the linked PR — its `Refs #N`, which `version-control` writes, and the PR
  link in your In PR transition comment.

A standalone issue has none. Set no link and post no comment saying so.

**State.** State is two values, moved in step: the `Status` field on the
tracking project (Project #45 in trusty-tools — `TICKETING.md` names the
project and maps each Status to its label) and the matching `status:*` label.
Move both in the same pass at every transition: filing, claim, PR open, merge,
verification, close. A Status with no label in that map (Backlog, Ready for
Development, Published) carries no `status:*` label. A repository with no
project Status field tracks state by the label alone.

```bash
tm issue transition N status:coded                     # the label
gh project item-edit <project#> --owner <owner> \
  --url <issue-url> --field Status --value "In PR"     # the field (gh 2.98+)
gh issue view N --json milestone,projectItems,labels   # read both back
```

**Before you report, run this checklist on every issue you filed or touched:**

<!-- rule:tm-ticketing/status-and-label-name-one-stage -->
```
□ Milestone set — or `no-milestone:` posted and a proposal in the report
□ Parent, sub-issues, blocked-by, blocking and linked PR recorded natively, where they exist
□ Project Status and `status:*` label name the same stage
□ `tm issue audit <N>` run, its output pasted
□ Report states the issue's milestone, relationships and state, one line each issue:
  #5001 — milestone "Backlog · search/core"; sub-issue of #4990, blocked-by #4988; Status In Development / status:in-progress
```

## The Labels, Project, Milestone Standard

<!-- rule:tm-ticketing/labels-project-milestone-standard -->
🔴 **Every issue and every pull request carries the same four things: the
`ws/<session>` label, its component label(s), its project, and its milestone.**
One standard, two artifacts. This section is where it is stated; `ticketing.md`,
`version-control.md` and `tm-workflow` carry the `gh` mechanics and point back
here by name rather than restating it.

| | Issue | Pull request |
|---|---|---|
| `ws/<session>` label | the filing session's | the opening session's |
| Component label(s) | the crate the defect lives in, read off the file paths the finding cites | every crate the PR's own diff touches, read off `git diff --name-only <base>...<head>` |
| Project | chosen by crate/topic fit, or `default_project` | the project(s) of the issue its `Refs #N` names |
| Milestone | by the ordered rule in "Choosing the Milestone and the Project (issues)" | the milestone of the issue its `Refs #N` names |
| Type / priority label | yes — "Labels" | no; a PR's type is its title's conventional-commit prefix |
| Relationships | native sub-issue and blocked-by — "Relationships (native, never prose)" | none; a PR's relationship IS its `Refs #N` |

Neither half of a PR's derived metadata is typed by anyone. `tm pr open`
computes both — the component labels from the diff, the project and milestone
from the `Refs` issue — and applies them in one `gh pr edit` after the PR
exists. A PR whose body carries no `Refs #N` gets no project and no milestone,
and the command prints one line saying so; a PR no workspace crate owns
(docs-only, CI-only) gets no component label and prints the same kind of line.
Both are the correct outcome, not a gap to fill by guessing.

🟡 **`trusty-mpm` means different things on the two artifacts, and this is the
one place that difference is stated.** On an issue it is a component label like
any other, applying only when the code at fault sits under `crates/trusty-mpm/`.
On a pull request it is the convention label every `tm pr open` attaches, which
is what marks a PR as a trusty-mpm session's. A PR therefore carries
`trusty-mpm` plus whatever component labels its diff earns, and those can be
disjoint.

## Related Skills

- `tm-workflow` — the delivery chain this issue lifecycle sits inside, and the ticketing/version-control boundary
- `tm-circuit-breaker` — CB#6 enforcement detail
- `tm-delegation-patterns` — where ticketing fits in the broader agent matrix
- `tm-bug-reporting` — the MCP-native path for daemon-captured errors
