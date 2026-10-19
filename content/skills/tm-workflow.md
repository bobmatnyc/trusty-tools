---
name: tm-workflow
description: The single trusty-mpm delivery workflow — phases and gates, the ticketing/workflow/version-control ownership boundary and handoff, worktree and branch discipline, changelog, PR body, review gate, squash-merge, cleanup, and how a project customizes the workflow via CLAUDE.md
user-invocable: true
metadata:
  version: "2.2.0"
category: pm-workflow
tags: [workflow, delivery-chain, pr, branch-protection, worktree, changelog, customization, verification-gates, pm-required]
effort: medium
---

# tm-workflow — The Delivery Workflow

<!-- #5202: this skill absorbed the retired `tm-pr-workflow`. There is one
     workflow skill. Issue policy lives in `tm-ticketing`; git and PR API
     mechanics live in the `version-control` agent. -->

This is the **sole workflow authority**: the complete sequence from an accepted
outcome to a merged PR and a closed issue, plus the gates applied along the way
and the mechanism a project uses to customize any of it.

It states policy and performs no operations. It never creates or mutates an
issue, a branch, a commit, or a PR — those are delegated, per the boundary
below.

## Ownership Boundary — Route by Artifact, Not by Verb

| Surface | Owns | Does not own |
|---|---|---|
| Core PM instructions | Detect the work shape, load this skill and `tm-ticketing`, preserve user authority, route the resulting actions | Tracker, git, or PR mechanics |
| **`tm-workflow`** (here) | The delivery sequence and its policy: phases/gates, worktree/branch conventions, changelog, PR body requirements, review, merge, cleanup, handoffs | Creating or mutating issues, branches, commits, or PRs |
| `tm-ticketing` + the `ticketing` agent | Whether an issue should exist; search/dedup; issue title and body; labels, assignee, milestone, comments, state, parent/child links | Any git operation, and any PR mutation — including PR title and body |
| The `version-control` agent | Branch/worktree/commit/push mechanics and the whole PR lifecycle: create, title/body (a phase PR's title is `<type>(<scope>): [EPIC_<epic#> PHASE_<n>] <subject>`, tagged by `tm pr open` — `tm-epic`), issue links, reviewers, checks, update-branch, arming auto-merge, the merge into main, post-merge verification against the exact head SHA, and reclaiming merged worktrees and their local branches | Authoring workflow policy, deciding whether an issue is warranted, changing issue metadata or state — including the `status:` advance its own merge just made due |

**The rule is the artifact, not "bookkeeping versus mechanics."** Every Issue API
operation (`gh issue …`, `mcp__mcp-ticketer__*`, `aitrackdown`) routes to
`ticketing` under P6. Every Pull Request API operation (`gh pr …`, including
`create`, `edit`, `view`, `list`, `diff`, `review`, `checks`, `update-branch`,
`merge`) and every git operation routes to `version-control` under P7. A PR body
is part of the PR, so it is version control's — splitting one `gh pr edit` across
two agents was the defect this replaced.

### The Handoff (stated once, here)

1. **PM → `ticketing`**: the finding or accepted outcome. Ticketing runs its own
   promotion and dedup gate and returns the canonical issue ID/URL, the outcome
   statement, and the closure conditions.
2. **PM → `version-control`**: the work summary, files changed, test evidence,
   the review verdict, and the issue context ticketing returned. Version control
   writes the PR body and inserts the correct issue link — `Refs owner/repo#N`
   by default, and a closing keyword only on a project whose `CLAUDE.md`
   permits a merge to auto-close the issue.
3. **After merge, PM → `ticketing`**: the merged PR number and squash SHA, for
   the completion comment and the `status:` advance. `version-control` confirms
   the merge and FLAGS the advance it owes — "#4409 owes `status:coded` ->
   `status:merged`" — because it may not make that edit itself. The PM routing
   that flag to `ticketing` is the only thing that keeps the label from going
   stale, since auto-merge lands PRs with nobody watching.

🔴 At every step, each issue carries a milestone, relationships and state —
`tm-ticketing`, "Every issue: milestone, relationships, state".

**Neither specialist delegates to the other.** The PM carries context between
them. A `ticketing` dispatch that ends "…then have version-control open the PR"
is a routing error.

## The Full Delivery Chain

```
accepted outcome -> optional issue -> worktree branch -> one cohesive PR
                 -> applicable gates -> trusty-review gate
                 -> squash-merge -> worktree cleanup -> issue closed
```

The chain starts at an **accepted outcome**, not at a ticket. The issue step is
**optional** for docs/CI/chore work and for a small fix the user explicitly asked
for that completes in one PR. It stays **required** for features, reproduced
defects, security work, cross-release dependencies, and any work that must
survive the current session. Whether a given finding earns an issue at all is
decided by the promotion gate in `tm-ticketing`, not here.

Every other step matters. Skipping the worktree step or the review gate is a
protocol violation the PM must not take on the user's behalf unasked.

## One Outcome, One PR

A PR contains one primary outcome and everything required to make that outcome
safely shippable: implementation, regression tests, necessary refactoring,
documentation/API updates, the changelog fragment, and in-scope review fixes.

- Do not split those artifacts into separate PRs because different agents
  produced them. The engineer's code, the QA agent's test, and the doc update for
  one outcome are one PR.
- **One PR may satisfy several tickets** when one coherent change resolves
  them (`Refs #A`, `Refs #B` — a closing keyword only where the project's
  `CLAUDE.md` permits merge-based auto-close, see "Minimal PR Body" below).
  Prefer that over several coupled PRs with an artificial merge order.
- Split only when the outcomes can be reviewed, deployed, or reverted
  independently, or when risk or size makes a stack materially safer to review.

**Bugfix batching is the default, not an exception.** Several open bugs
constrained to the same file, module, or tightly-coupled area go in ONE PR —
one engineer, one worktree, one review, one CI pass — not one PR per issue.
Each issue keeps its own regression test and its own `Refs #N` in the PR body.
One changelog fragment per crate may cover the whole batch, naming each issue.
Unrelated bugs in different modules still get separate PRs. This is what "no
two in-flight PRs share a module" means for a bug cluster: same module now
means same PR, not a serialized queue of small ones.

## Where Each Task Lives

This file holds the rules every delivery needs. Everything else sits in one file
per task under `references/`, beside this file. Read the file your task needs,
and only that file. A pointer elsewhere that names a section of `tm-workflow`
resolves here: find the section name in the middle column.

| Task | Sections | File |
|---|---|---|
| Pick the phases a task runs, brief each phase, honour an override | "The 5-Phase Model and Its Dispatch Briefs", "Sprint, then Harden — the Rest of the Doctrine" | [references/phases.md](references/phases.md) |
| Decide whether work is done and which findings block it; do deterministic work deterministically | "Completion Standard — the Full Text", "Deterministic Work" | [references/completion-standard.md](references/completion-standard.md) |
| Choose how wide a test gate runs at each stage | "Test Scope Widens by Stage" | [references/test-scope.md](references/test-scope.md) |
| Provision a worktree, keep the main checkout fresh, isolate a dispatch, clean up after merge | "Worktree Discipline (Mandatory Before Any Source Edit)", including "Keep the main checkout fresh"; "Task-Owned Cleanup" | [references/worktree-discipline.md](references/worktree-discipline.md) |
| Read another session on this repo; own, check or hand off the merge queue | "Concurrent Sessions on One Repo", "Merge-Queue Ownership — the Procedure" | [references/concurrent-sessions-and-merge-queue.md](references/concurrent-sessions-and-merge-queue.md) |
| Write the changelog entry a PR owes | "Changelog Requirement (Before Merge)" | [references/changelog.md](references/changelog.md) |
| Handle a red gate the branch did not cause; run the review gate | "Baseline-Failure Protocol (Red That Isn't Yours)", "The trusty-review Gate" | [references/baseline-and-review.md](references/baseline-and-review.md) |
| Customize the workflow through `CLAUDE.md`; inspect the resolved prompt; pick an output style | "How the PM Prompt Is Assembled", "The One Customization Surface", "Trigger Phrases", "Inspecting the Resolved Prompt", "Output Style Selection (#8533)" | [references/customizing.md](references/customizing.md) |
| Open these files from an agent with no `Skill` tool | the Read path | [references/README.md](references/README.md) |

## Git Security Review (Mandatory Before Push)

Before any `git push`, delegate a credential scan to the `security` agent.

🔴 **`git status --porcelain` must read empty before you run any gate below
(#7739).** A gate run against a dirty tree proves the working copy, not the
ref a push actually ships; commit every post-rebase edit first, so the ref
gated is the ref pushed.

0. **Verify the base ref before anything diffs it (#7748).** Every range below
   reads the LOCAL `refs/remotes/origin/main`, which a checkout that has not
   fetched can carry hundreds of commits behind — twice on 2026-09-13, and one
   of those would have scanned ~1,270 unrelated paths. Compare `git rev-parse
   refs/remotes/origin/main` with `git ls-remote origin refs/heads/main`, and
   `git fetch origin main` when they disagree. A comparison that cannot be made
   (no remote, no network) refuses the scan naming both shas; it never reports
   the scan as passed. `tm pr open` runs this same gate over its own diffs.

1. **`gitleaks` is the primary check**: `gitleaks git --log-opts="origin/main...<branch>"`
   (verified working, gitleaks 8.30.1). Three-dot, same reason as below — it
   scopes the scan to commits your branch added. Caveat: a three-dot range
   passed to `git log`/`--log-opts` is symmetric-difference, not merge-base-to-HEAD
   — it can also surface commits that only exist on `main`'s side. Treat any
   hit outside your own commits as a false positive to confirm, not a block.
2. `git diff origin/main...HEAD` is a **secondary** signal — the diff about to
   be pushed. Three-dot, because it diffs from the merge base and shows only
   what YOUR branch changed. Two-dot compares the two commits, so files DELETED
   from `main` since your branch point come back as your additions: a measured
   run reported 19 hits that were another PR's deletions where three-dot
   reported zero across 36 files. A scan people learn to wave through is where
   a real secret hides.
3. **`detect-secrets` run against paths, not diff text, is a silent no-op on a
   branch that isn't checked out** — it scans the working tree on disk, so a
   scan invoked against a branch other than the one currently checked out finds
   nothing and reports clean regardless of content.
4. **The sandbox refuses any path containing the word "secrets"** — a
   `detect-secrets` invocation naming its own tool or config path can trip this
   and abort before it scans anything. Route the scan through `gitleaks`
   instead when that happens.
5. `security` scans the gitleaks output and the diff for API keys, passwords,
   private keys, and tokens, and returns either clean or the list of blocked
   items. Its report states `git diff --name-only --diff-filter=d <range> |
   wc -l` and the files covered (#8504). Files covered are the files whose
   text hunks it read, plus the files with no text hunk (pure rename,
   mode-only, binary), listed separately with each binary named for manual
   review. Counts that differ make the scan INCOMPLETE, never clean:
   re-dispatch it over the unscanned files.
6. **Block the push if secrets are detected.** A leaked credential in git history
   survives the commit being reverted.

## Branch Protection

All pushes to `main`/`master` require a feature branch + PR — no exceptions,
including "trivial" docs/chore/typo work (which may skip the linked-issue step
but still requires branch + PR + squash-merge). The only bypass is release
tooling (version-bump / lockfile-update commits per the release workflow), and
only for those commit types.

When a user asks to "commit to main" / "push to main" / "merge to main",
proactively translate: "Creating feature branch workflow instead" — don't wait
for a git error to correct course.

## Minimal PR Body (nine fields)

The `version-control` agent writes this; the PM supplies the material.
`tm pr open` checks each field against its own heading below, verbatim
(`#7727` — every heading must be an ATX `## ` line, one to six `#`s, matched
case-insensitively against the canonical word or a stated alias):

🔴 A rung 1 (docs-only) branch needs `tm pr open --docs-only` explicitly — it
runs the changelog-fragment gate and refuses a docs-only PR without the flag.

🟡 A failure naming a missing heading prints the whole skeleton verbatim, ready
to paste and fill (#7574). On a project whose own `CLAUDE.md` names a different
PR-body standard, `--minimal` skips these nine headings and nothing else —
the attribution footer, the `Refs`/`Closes` rule and the changelog-fragment gate
all still run (#7615). It is never the answer on THIS repo, whose standard these
nine fields are.

1. `## Outcome` — primary outcome and linked issue(s), with the
   `Refs owner/repo#N` link.
2. `## Changes` — what changed, and what is intentionally out of scope.
3. `## Risk` — risk / blast radius.
4. `## Tests` — test evidence at the applicable levels.
5. `## Baseline` — baseline/pre-existing failures and their canonical issue
   (see `references/baseline-and-review.md`).
6. `## Gates not run` — every gate the rung asked for that you did NOT run, and
   why (#7336). A provider call automatic approval refused, a suite you skipped
   for time, an `--include-ignored` run you left out.
7. `## Partial-red accounting` — each target still failing at hand-back, with
   its per-target rerun result (#7336). "This was not an unbroken green
   aggregate run", itemized.
8. `## Docs` — documentation/changelog status.
9. `## Review` — review-finding disposition: fixed here, kept on the parent,
   or separately ticketed.

🔴 **Fields 6 and 7 are answered, never omitted.** A clean run writes the single
word `none` under each — that is a valid whole section, and it is the disclosure
(#7336). Silence is not: `tm pr open` exits 2 naming the missing field, and the
line it prints says `none` is valid.

🔴 **Field 1 models `Refs`, not a closing keyword (#6895).** `Closes`, `Fixes`
and `Resolves` go in only when the project's `CLAUDE.md` permits a merge to
auto-close the issue. On a Refs-only project — one whose `CLAUDE.md` says fix
PRs use `Refs #N` and never `Closes #N` — every issue reference in the body is
`Refs`, because an issue there closes from `status:tested` after live
verification, not from the merge.

GitHub scans the WHOLE body for a closing keyword, not just field 1, and
honours one inside a negation ("does not close #N" still closed it, #5389). So
a single `Fixes #N` in any field auto-closes the issue on squash-merge before
anyone verified the fix — that is exactly what PR #6894 did to #6888. Grep the
finished body for a keyword before opening the PR and after every body edit;
`version-control` carries the command.

**PR body freshness**: when scope or claims change mid-flight, delegate the
update to `version-control` immediately rather than leaving stale assertions.

## Squash-Merge Is Required

PRs merge via **squash-merge only** — one clean commit on `main` per PR. Delete
the feature branch immediately after. No merge commits, no rebase-merge, for PRs
landing on `main`.

```bash
tm pr merge <PR>                                  # validated body becomes the squash commit (#6808)
tm pr merge <PR> --auto                           # arm auto-merge; waits only on running required checks
tm pr cleanup <PR>                                # remote ref, worktree, local branch
gh pr merge <PR> --squash                         # fallback on a host without `tm` — no --delete-branch, no checks gate
```

🔴 **The checks gate (#8614).** `tm pr merge` refuses, with or without
`--auto`, while any check has failed, while no check has registered (unless
`--auto` and a check is required), or while a check it would not wait for is
still running. Only `--auto` waits, and only on
required checks. It reads the required checks from the base branch's
protection and its rulesets; a failed read refuses. Two escapes exist:

- `--allow-failing <check>` (exact name, repeatable) waives one non-required
  check, failing or running. It never waives a required check. The waiver
  heads the squash commit body. Pass it only for a check the brief waives by
  name as pre-existing or unrelated, never for a branch-caused failure.
- `--allow-no-checks` merges a PR on a repository with no CI.

A refusal is a blocker to report. Never route around it with raw
`gh pr merge` or `--admin`; an admin-merge authorization covers the review gate
only. The version-control report names the brief clause that authorized the
merge, and flags a base branch with no required checks as a risk. Before the
raw-`gh` fallback, one `statusCheckRollup` read must list at least one check
(or the brief says the repo has no CI) and show no failed or running check
outside the brief's waivers.

🔴 **Never assume auto-merge is available (#8640).** `version-control` reads
`gh api repos/<owner>/<repo> --jq .allow_auto_merge` before planning a merge
and reports it up front (#9747). An empty answer means the token cannot read
the setting and counts as `false`. When it is `false`, `--auto` fails with `Auto merge is not allowed
for this repository (enablePullRequestAutoMerge)`, and the merge is a direct
`tm pr merge <PR>` once every check has settled. A brief that orders `--auto`
does not change that answer.

`--delete-branch` fails post-merge whenever a worktree holds the base branch
(#7104) or the head branch (#8391) — every `isolation: "worktree"` delivery
holds the head, so treat this as the common case, not the exception. `tm pr
merge` already tolerates the failure and exits 0; `tm pr cleanup <PR>` finishes
the job. See version-control's delete-branch sequence for the fallback without
`tm`.

After a squash-merge the local feature branch shows as "unmerged" to git (the
squashed commit has a different hash). That is expected, not a failed merge —
but it also means `git branch -d`/`-D` cannot rely on ancestry. Before
deleting, `git fetch origin`, then confirm `git merge-tree --write-tree
origin/<base> <head>` equals `git rev-parse origin/<base>^{tree}`; never
compare the tip's tree against the squash commit's tree, which differ
whenever `<base>` advanced after the merge.

## Shipped Defaults on the PR

The rule is `tm-ticketing`'s, in the section
**"The Labels, Project, Milestone Standard"** — one standard over issues and
pull requests. Read it there; it is not restated here or in a
`version-control` brief.

What `tm pr open` reads to satisfy the PR half, and where each value comes from:

| Field | Source |
|---|---|
| assignee | `agents.ticketing.default_assignee` |
| `trusty-mpm` label | the convention label, on every PR |
| `ws/<session-name>` label | `$TM_SESSION_NAME`, else `tmux display-message -p '#{session_name}'` |
| component label(s) | `git diff --name-only origin/<base>...HEAD`, mapped to the workspace's crates |
| milestone, project(s) | the issue the body's first `Refs #N` names |
| attribution footer | the `attribution` key tm writes into the provisioned Claude Code settings |

The first three are attached to `gh pr create` and the command refuses to call
`gh` at all until they are in place. The next two are applied by one `gh pr
edit` after the PR exists, best-effort: a failure warns, and a PR with no
`Refs #N` or a diff no crate owns prints the line naming what was skipped. A
workstream is a **label**, never a milestone.

Each of these steps retries once before it gives up. If a field is still
missing after the retry, `tm pr open` exits 3 (`EXIT_PARTIAL`, #7869) rather
than 0 — the PR itself was created, and the printed line names its number,
URL, and the field(s) still missing, so finish them with `gh pr edit` instead
of treating the run as failed.

```bash
gh label create trusty-mpm \
  --description "Created/managed by a trusty-mpm session" --color 8250df \
  2>/dev/null || true
```

## Delegating the PR

PR creation and every later PR edit go to `version-control` — branch push,
`gh pr create`, the body, the issue-closing link, reviewers, `gh pr checks`, and
the merge. Give it: work summary, files changed, test status, the trusty-review
verdict, the canonical issue context from `ticketing`, the shipped defaults
above, and every check it may merge over, by exact name and reason (see "The
checks gate"). The PM constructs the brief; it never runs `gh pr` itself (P7 /
CB#6).

## Customizing the Workflow

trusty-mpm has no separate "workflow engine" a project configures at runtime. The
PM's phase/gate behavior comes from the bundled instruction package, and a
project customizes it via named-section marker blocks in its root `CLAUDE.md`.
This is implemented in `core/instruction_overrides.rs`,
`core/instruction_pipeline.rs`, and `core/claude_md_sections.rs`.

### The Safety Core (#8533)

Every section is replaceable except a small safety core. The core is
enumerated once in code, `core/instruction_safety_core.rs::SAFETY_CORE`; a test
fails if this table and that list diverge.

<!-- safety-core:start -->
| Member | Section token | Kind |
|---|---|---|
| Memory & Instruction Sources | `CORE` | fixed section |
| Customization Surface | `CORE` | fixed section |
| Completion Standard | `CORE` | fixed section |
| Detected project stack | `CORE` | generated block |
| Memory protocol | `MEMORY` | pinned block |
| Code search protocol | `SEARCH` | pinned block |
| Agent selection | `AGENT-DELEGATION` | pinned block |
| Agent roster | `AGENT-DELEGATION` | generated block |
<!-- safety-core:end -->

- A **fixed section** declines a marker naming it. The decline is logged and
  reported, and the bundled text stays.
- A **pinned block** survives an override of its section; the project's text
  replaces the rest of that section.
- A **generated block** is computed at launch; no override can author or
  remove it.

There is no other floor: the `is_floor()`/`instruction_floor.sha256` machinery
was retired by #4286, being the appearance of a control rather than one a
project-owned `CLAUDE.md` could enforce.

At session start, `core/instruction_overrides.rs::resolve_pm_prompt` (reached via
`build_system_prompt_for*`) composes the final prompt. It is not a file a user
edits — it is composed fresh per launch.

## Related Skills

- `tm-ticketing` — whether an issue exists, and everything about its content and lifecycle
- `tm-git-file-tracking` — files must be tracked on the feature branch before PR creation
- `tm-delegation-patterns` — the agent-selection matrices this workflow routes into
- `tm-circuit-breaker` — CB#5 (delegation chain), CB#6 (forbidden `gh` usage), CB#8 (QA/review gate)
- `tm-verification-protocols` — the QA evidence standard every phase gate uses
- `tm-agent-architecture` — how the agents this workflow delegates to are built
