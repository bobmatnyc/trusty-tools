---
name: tm-issues-prune
description: Audit, prune, align, organize, prioritize, and build delivery views from a project's GitHub issues, labels, milestones, and Projects — natural-language PM delegation pattern (gh-first, JIRA deferred)
user-invocable: true
version: "2.0.0"
category: pm-workflow
tags: [tickets, github, backlog, pm-required, triage, prioritization]
effort: medium
---

# /tm-issues-prune — Portfolio Audit, Align, Prune & Prioritize

A natural-language PM-delegation pattern for keeping a project's GitHub
issue portfolio healthy and actionable: validating tickets against current
code/specs, consolidating stale/duplicate/obsolete issues and labels, aligning
milestones to active/backlog/paused delivery lanes, grouping survivors into a
navigable map or GitHub Project, producing a deterministic priority ranking,
and surfacing a short list of next tasks. This skill describes a workflow, not
a new tool or command — every mechanical `gh` operation is delegated, never run
by the PM directly.

The full sweep runs in six phases: **Audit → Prune → Align → Organize →
Prioritize → Suggest Next**. Later phases operate on the verified surviving
set, so stale metadata does not pollute delivery views or ranking.

## Scope: gh-first, JIRA deferred

This skill operates on **GitHub Issues only**, via the `gh` CLI. JIRA is
**not yet supported** — `TicketSystemKind::Jira` is stubbed in
`trusty-agents`, so there is no working JIRA backend to prune/prioritize
against today. The workflow below is written so a future JIRA backend can
slot in without changing the PM-facing shape (prune candidates → confirm →
close; prioritize → apply labels), but until that backend exists, only
GitHub-repo projects can use `/tm-issues-prune`.

## Delegation Pattern

**The PM never runs `gh issue` commands directly.** Delegate the mechanical
work to the `ticketing` agent, which shells `gh issue`, `gh project`, and the
relevant API calls against the active project's GitHub repo. Both single-issue
and bulk portfolio operations belong to ticketing; Version Control owns the PR
artifact and git operations. A prune/prioritize sweep is workflow-state
intelligence across the whole backlog, backed by `gh` instead of
`mcp-ticketer`/`aitrackdown` for this repo's own issues.

**Wrong:**
```
PM: gh issue list --state open --json number,title,updatedAt,labels
PM: gh issue close 1234 --comment "stale"
```

**Correct:**
```
PM: "I'll have ticketing survey the open backlog for prune/priority candidates..."
[PM constructs a delegation prompt for the ticketing agent, scoped to this repo]
[ticketing agent runs gh issue list/close/edit, returns structured findings]
PM: [presents the prune/prioritize summary, asks for confirmation before closing]
```

## `/tm-issues-prune` Subcommands

| Subcommand | Purpose |
|---|---|
| `/tm-issues-prune scan` | Survey the backlog, report prune candidates AND priority gaps without changing anything |
| `/tm-issues-prune close` | Prune pass — close evidence-backed candidates when explicitly authorized; otherwise present for confirmation |
| `/tm-issues-prune align` | Align labels and milestones into explicit active/backlog/paused delivery lanes |
| `/tm-issues-prune organize` | Organize pass — group surviving open issues by epic/component/theme, flag orphans |
| `/tm-issues-prune prioritize` | Prioritize pass — assess open issues, propose label changes, surface a ranked list |
| `/tm-issues-prune suggest-next` | Suggest-next pass — recommend the top 3-7 next tasks as a selectable table |
| `/tm-issues-prune project-build` | Create or repair a minimal GitHub Project view over the aligned issue set |
| `/tm-issues-prune` (no args) | Run Audit → Prune → Align → Organize → Prioritize → Suggest Next; Project build remains explicit |

## Prune Workflow

1. **Delegate the survey.** Ask the `ticketing` agent to resolve the default
   branch and pull the paginated issue/PR, label, milestone, and Project
   inventory for the active repo. Do not assume one `--limit` result is the
   whole corpus:
   ```bash
   gh issue list --state open --limit 1000 \
     --json number,title,updatedAt,labels,body,comments
   gh api --method GET --paginate \
     'repos/{owner}/{repo}/milestones?state=all&per_page=100'
   gh project list --owner '{owner}'
   ```
2. **Classify candidates** against these criteria (the ticketing agent
   applies them, the PM does not re-derive them):
   - **Stale** — no activity (comments, commits, label changes) in more
     than N days (default N=90; PM confirms or overrides N with the user
     before delegating).
   - **Duplicate** — title/body substantially overlaps an existing open
     issue; link the surviving issue number.
   - **Obsolete/superseded** — current default-branch code, tests, specs/ADRs,
     and history show the requested outcome already landed or no longer exists.
     A commit that merely mentions the issue is a lead, not sufficient proof.
   - **Won't-fix** — valid report but explicitly out of scope or rejected
     in a prior comment thread.
3. **Present the candidate list WITH reasons**, one line per issue. Staleness
   alone is never enough to close. If the user explicitly asked to close,
   consolidate, prune, or clean the tracker, that request authorizes the
   evidence-backed candidates after the exact target/rules are restated. A
   read-only scan or vague "take a look" does not; request confirmation before
   closing in that case.
4. **On confirmation**, delegate the close pass. Each close carries a
   reason comment, not a bare state change:
   ```bash
   gh issue close 1234 --comment "Closing as stale — no activity since \
   <date>. Reopen if still relevant."
   gh issue close 1235 --comment "Duplicate of #1200 — consolidating \
   discussion there."
   ```
5. **Report** the final close list back to the user with issue numbers and
   reasons, so the action is auditable after the fact.

## Align Workflow — Labels and Milestones

Run after pruning and before grouping/ranking.

1. **Define deterministic lanes.** Derive track precedence from established
   component labels/titles/spec ownership. For each track, distinguish active
   release cleanup, unscheduled backlog, and explicitly paused work. Record
   feature exclusions for any no-new-features milestone.
2. **Consolidate labels safely.** Identify semantic aliases and obsolete
   execution labels. Add the canonical label to every affected item, verify
   coverage, then delete/deprecate the source. Never delete after a partial
   migration or transient API error.
3. **Build destinations first.** Reuse matching open milestones. Create or
   rename milestones only under an explicit roadmap-organization request.
   Give every milestone a goal, exclusions, and observable exit criteria.
4. **Classify conservatively.** Bugs/regressions/security/CI/tests/docs/
   packaging/bounded maintenance may enter release cleanup. Enhancements,
   epics, feature-titled work, experiments, and ambiguous items go to backlog.
   Paused components override all other classifications.
5. **Reconcile PRs.** Milestone `open_issues` includes PRs while
   `gh issue list` excludes them. Inspect and move open PRs before closing an
   apparently empty legacy milestone.
6. **Verify invariants.** Report unmilestoned count, active-scope violations,
   label coverage, open milestone list, and failed/retried mutations.

## GitHub Project-Build Workflow

Use only when the user explicitly asks for a board/Project or needs multiple
planning axes/cross-repository visibility. Issues remain canonical.

1. Inventory `gh project list` and `gh project field-list`; reuse a matching
   Project rather than creating a parallel board.
2. Keep fields minimal: built-in `Status`, then stable `Track`, `Delivery lane`,
   `Priority`, and `Target release` only where they drive decisions.
3. Add existing issue/PR URLs with `gh project item-add`. Draft items are only
   for intentionally pre-ticket work.
4. Resolve Project/item/field/option IDs before `gh project item-edit`; never
   guess display names into ID parameters.
5. Prefer three decision views: active release by track, backlog by track, and
   paused. Do not create views per label or session. Saved-view configuration
   may not be writable through the available CLI/API; use an authorized UI tool
   or report the exact remaining manual step instead of claiming success.
6. Verify item count and field coverage; report orphans rather than silently
   dropping them.

## Prioritize Workflow

1. **Delegate the assessment.** Ask the `ticketing` agent to pull open
   issues with their current labels:
   ```bash
   gh issue list --state open --limit 500 --json number,title,labels,createdAt
   ```
2. **Classify against priority evidence.** Propose a P0/P1/P2/… label only when
   the issue text, a maintainer decision, or objective security/data-loss signal
   asserts severity. Age, linked PR activity, and blocking relationships affect
   the ranked list but do not manufacture a priority label. Distinguish:
   - **Unlabeled** — no priority label at all.
   - **Mislabeled** — current label looks inconsistent with the issue's
     actual signal (e.g. a security bug labeled P3).
   - **Correctly labeled** — no change needed.
3. **Present the top-N** (default N=10) most important open issues to the
   user, with the proposed label change and a one-line justification for
   each.
4. **On confirmation** (or for a lower-stakes "add labels, don't remove
   anything" pass, which does not need the same close-level confirmation
   gate — but still summarize before/after), delegate the label edits:
   ```bash
   gh issue edit 1234 --add-label "P1" --remove-label "P3"
   gh issue edit 1235 --add-label "P0"
   ```
5. **Report** the final label changes and the surfaced top-N list.

## Organize Workflow

Runs after the prune-close pass (if any) so grouping only covers issues
that actually survive. Ask the `ticketing` agent to pull the surviving
open-issue set with labels and bodies, then group into a map along three
axes:

1. **Epic** — group by parent issue where the repo has an epic/parent-issue
   convention. Detect this from: an `epic` label, a body reference like
   `Part of #N` / `Epic: #N`, or a parent issue whose body lists child issue
   numbers. If the repo has no established epic convention, say so rather
   than inventing one.
2. **Component/crate** — infer from conventional-commit-style title
   prefixes (`feat(trusty-mpm): …`, `fix(trusty-search): …`, etc.) and from
   any `crate:*` / component labels already applied.
3. **Theme** — when an issue has no inferable epic and no inferable
   component, cluster it with similar issues by subject matter (e.g.
   "installer/signing", "session lifecycle") based on title/body content.

**Orphans** — issues with no epic, no labels, and no inferable component or
theme — must be called out explicitly in their own section, e.g.
`Orphans (needs labeling): #1234, #1240`. Do not guess a component/theme
for an orphan just to force it into a bucket; ask the user to label it
instead.

Present the result as a grouped map (epic → component → theme → orphans),
one line per issue with its number and title, so the user can see the
backlog's shape before ranking.

## Prioritize (Ranked List) Workflow

This is a deterministic ranking pass over the organized, surviving open
issues — distinct from the label-correction "Prioritize Workflow" above,
which proposes label *changes*. This pass produces an **ordered list**
using only facts pulled via `gh` (labels, cross-references, dates,
milestone state) — never subjective judgment alone. Apply these heuristics
in order, most authoritative first:

1. **Existing priority labels** — P0 > P1 > P2 > P3 > unlabeled.
2. **Blocks other work** — issues referenced by open PRs or other open
   issues via `Closes #`, `Blocks #`, `Depends on #`, or cross-references
   visible in `gh issue view --json` / `gh api` linked-PR data. An issue
   that blocks active work outranks one that doesn't, within the same
   priority tier.
3. **Active epic/milestone membership** — an issue belonging to an open
   milestone with a due date, or an open parent epic issue that is itself
   still active, outranks an otherwise-similar issue with no active epic.
4. **Staleness** — old and untouched (no comments/updates in a long window,
   default 90 days unless the user specified another window during the
   prune pass) is a signal to **demote** or flag as a close-candidate for
   the next prune pass — never a reason to promote.
5. **Size hints** — if the issue is labeled/estimated for size, use it only
   as a final tiebreaker between otherwise-equal issues.

**Output format:** an ordered list, one line per issue, ending in a short
rationale citing which heuristic(s) fired, e.g.:

```
1. #1234 "Fix daemon restart race" — P0 label; blocks #1240, #1255
2. #1201 "Add retry backoff to proxy" — P1 label; active epic #1100
3. #1188 "Refactor config loader" — P2 label; stale 94d — demote candidate
```

State explicitly which `gh`-observable fact justified each ranking
decision. If two issues tie on every heuristic, say so rather than
inventing a tiebreak.

## Suggest Next Workflow

Runs last, over the ranked list, to recommend a short, concrete slate of
next tasks:

1. **Select 3-7 candidates** from the top of the ranked list, adjusted for
   dependency sequencing: this repo's layer priority is API/daemon routes
   before CLI before TUI/GUI — i.e. prefer lower-layer work that unblocks
   higher-layer work over higher-layer work that has no such leverage, even
   if the higher-layer item ranks slightly higher in raw priority.
2. **Size each candidate** S/M/L (rough effort, not a strict estimate).
3. **Write a one-line why-now rationale** per candidate (why this, why
   now — cite the ranking heuristic(s) that put it near the top).
4. **Note what it unblocks** — which other open issues or PRs become easier
   or possible once this lands (cross-reference the Organize/Prioritize
   output; don't invent unblocks that aren't visible in the `gh` data).
5. **Present as a markdown table:**

   | # | Title | Size | Why now | Unblocks |
   |---|---|---|---|---|
   | #1234 | Fix daemon restart race | M | P0, blocks 2 other issues | #1240, #1255 |

6. **Do not auto-start any of these tasks.** Present the table to the user
   and wait for them to explicitly choose one (or more) before delegating
   any implementation work.

## Output Format (full sweep)

When running the full `/tm-issues-prune` sweep (no args, all phases), report
back to the user in this sequence, each section clearly labeled:

1. **Audit summary** — repository/default branch, inventory counts, and current
   taxonomy/project findings.
2. **Prune summary** — candidates presented, authorization/confirmations, final
   close list with reasons (empty if nothing was closed).
3. **Alignment summary** — label merges, milestone moves, active-scope checks,
   and any intentionally unassigned items.
4. **Organized map** — epic → component → theme → orphans, one line per
   surviving open issue.
5. **Ranked list** — the ordered list from the Prioritize (Ranked List)
   Workflow, each line with its rationale.
6. **Next-task table** — the Suggest Next markdown table, ending with an
   explicit prompt for the user to pick a task (or none).

Any phase can also be run standalone via its subcommand (`organize`,
`prioritize`, `suggest-next`) without running the full sweep, in which case
only that section is reported.

## Safety Rules

- **Closing needs authority and evidence.** An explicit user request to close,
  consolidate, prune, or clean supplies authority for the stated scope after
  exact candidates/rules are restated. Otherwise present candidates and wait.
- **Bulk mutations are staged.** Create destination metadata, migrate, verify,
  then remove source metadata. On partial failure, retain the source and retry
  only the missing items.
- **Priority label changes are additive/correctable**, not destructive, but
  still summarize proposed changes before applying them at scale (more than
  a handful of issues).
- If the user has not specified a staleness window, use 90 days for reporting;
  never close on age alone.
- Never delegate a prune/prioritize sweep against a repo other than the
  one the active project is registered against — confirm the target repo
  first if it is ambiguous.
- **Organize, ranked-list Prioritize, and Suggest Next are read-only** — they
  never close issues or edit labels, so they carry no confirmation gate.
  Suggest Next's only guardrail is behavioral, not destructive: never
  auto-start a suggested task without the user explicitly picking it.

## JIRA (Future)

JIRA is not yet supported: `TicketSystemKind::Jira` is stubbed in
`trusty-agents`, so there is no working JIRA backend today. A JIRA backend
is a future follow-up — when it lands, the same prune/organize/prioritize/
suggest-next shape applies, with the `ticketing` agent choosing the backend
by project configuration instead of always shelling `gh`.

## Related Skills

- `tm-ticketing` — the general ticket-driven-development protocol and the
  issue/milestone/Project policy this skill extends for bulk portfolio hygiene
- `tm-delegation-patterns` — where this fits in the broader agent matrix
- `tm-circuit-breaker` — CB#6 (forbidden direct tool usage) applies here too:
  the PM delegates, it never runs `gh issue` itself
