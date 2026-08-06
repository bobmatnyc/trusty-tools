---
name: tm-pr-workflow
description: Branch protection, per-PR changelog fragment requirement (changelog.d file format and category line), trusty-review merge gate, squash-merge, and worktree discipline for landing work on main
user-invocable: false
version: "1.0.0"
category: pm-workflow
tags: [git, pr, branch-protection, worktree, pm-required]
effort: medium
---

# PR Workflow: The tm-Specific Layer

Native Claude Code already knows how to run `gh pr create` — this skill does
not re-explain that mechanic. It covers the parts that are specific to this
repo's delivery discipline and are easy to get wrong: worktree isolation,
branch protection, the trusty-review gate, and the squash-merge requirement.

## The Full Delivery Chain (this repo's convention)

```
accepted outcome -> optional issue -> worktree branch -> one cohesive PR
                 -> applicable gates -> trusty-review gate
                 -> squash-merge -> worktree cleanup
```

The chain starts at an **accepted outcome**, not at a ticket. The issue step is
**optional** for docs/CI/chore work and for a small fix the user explicitly
asked for and that completes in one PR. It stays **required** for features,
reproduced defects, security work, cross-release dependencies, and any work that
must survive the current session. Whether a finding earns an issue at all is the
promotion gate in `tm-ticketing`.

Every other step matters. Skipping the worktree step or the review gate is not a
shortcut — it's a protocol violation the PM must not take on the user's
behalf without being asked.

## One Outcome, One PR

A PR contains one primary outcome and everything required to make that outcome
safely shippable: implementation, regression tests, necessary refactoring,
documentation/API updates, the changelog fragment, and in-scope review fixes.

- Do not split those artifacts into separate PRs because different agents
  produced them. The engineer's code, the QA agent's test, and the doc update
  for one outcome are one PR.
- **One PR may close several tickets** when one coherent change satisfies them
  (`Closes #A`, `Closes #B`). Prefer that over several coupled PRs with an
  artificial merge order.
- Split only when the outcomes can be reviewed, deployed, or reverted
  independently, or when risk or size makes a stack materially safer to review.

## Minimal PR Body (seven fields)

1. Primary outcome and linked issue(s).
2. What changed, and what is intentionally out of scope.
3. Risk / blast radius.
4. Test evidence at the applicable levels.
5. Baseline/pre-existing failures and their canonical issue (see below).
6. Documentation/changelog status.
7. Review-finding disposition: fixed here, kept on the parent, or separately
   ticketed.

## Baseline-Failure Protocol (Red That Isn't Yours)

When a required gate goes red, establish whose red it is before doing anything
else.

1. Never turn red green by removing coverage. That hard line is stated in the
   instruction package ("Sprint, then Harden") and is not re-specified here —
   it is the entry condition for the rest of this protocol.
2. Determine whether the branch caused the failure: run the same gate on the
   base branch, check the failing test's recent history, or reproduce it in
   isolation.
3. **Branch-caused** → fix it in this PR.
4. **Pre-existing and already tracked** → append the run URL, SHA, command, and
   failure signature to the canonical issue. Do not open another issue.
5. **Pre-existing and untracked** → open ONE canonical issue, and only after a
   reproduction or sufficient CI evidence. A single unrelated red run is an
   observation, not a ticket (`tm-ticketing`, promotion gate).
6. Report the gate result in exactly this shape:

   ```
   change-specific gates pass; <gate name> blocked by canonical issue #N
   ```

   Never report "all tests pass" while a gate is red, whoever caused it. Merge
   disposition then follows branch protection and the risk tier; a red required
   check is never merged around.

## Worktree Discipline (Mandatory Before Any Edit)

**Source of truth is `origin/main:HEAD`, not local `main`.** Local main can go
stale — always fetch before branching, and provision a dedicated worktree
before any edit, build, or test run:

```bash
git fetch origin main
git worktree add -b <feature-or-fix-branch> \
                  .claude/worktrees/<dirname> origin/main
cd .claude/worktrees/<dirname>
```

**The main checkout is inspection-only.** Read-only operations only —
`git status`/`log`/`diff`/`show`, file reads. Forbidden against it: any edit;
any build or test run (whatever the project's gate is — `cargo build`/`test`,
`npm run build`/`test`, `pytest`, ...); any destructive git operation (`git
reset --hard`, `git checkout .`, `git stash`, `git restore .`); any
file-mutating command (`sed`/`awk`/`patch`) — or anything else that mutates
the working tree, index, or build output.

**A worktree is a writer; the branch is the workstream.** The durable unit is
the branch — one branch per workstream, one session per workstream. A
worktree is only the checkout that lets you write to that branch: ephemeral,
disposable, recreatable at any time with `git worktree add`. Losing a
worktree loses nothing the branch does not still hold.

**One branch and worktree per independently reviewable PR outcome** — not per
ticket, per refactor step, or per experiment. Several related tickets may
share one worktree when a single coherent change satisfies them (`Closes #A`,
`Closes #B`); see "One Outcome, One PR" above for everything that outcome owes
and keeps bundled in that same worktree and PR. Experiments stay
session-local — promote one to a branch and worktree only once its result is
accepted for implementation.

**Cleanup, in order:** `git worktree remove --force <path>` once the PR has
merged, then `git branch -D <branch>` and `git push origin --delete <branch>`
— the branch goes last, because until the squash-merge lands it is the only
durable copy of the workstream. `git worktree remove` deletes only the
worktree directory; it never touches the main checkout or any other worktree.

**A command that genuinely must run from the main checkout** (a post-merge
install step, say) stashes first, operates, then restores:

```bash
git -C /path/to/main-checkout stash push -u -m "claude: pre-op-safety $(date +%s)"
# … do the op …
git -C /path/to/main-checkout stash pop
```

Surface the stash name in your report if popping fails, so the human can
restore manually. This is a narrow exception, not license for routine edits
from the main checkout.

**Every subagent dispatch must name the exact worktree path it is confined
to**, and must never leave it: no operating from the main checkout, no `git
reset --hard` / `git checkout .` / `git stash` against main, no touching
files outside the assigned worktree. "Operate from the main checkout" is a
banned instruction pattern in any dispatch prompt — QA agents get their own
worktree (e.g. `.claude/worktrees/qa-<ticket-or-pass>`) exactly like
engineering agents.

Project-specific worktree hazards (binary-install caveats, code-signing
caches, and the like) belong in the project's own reference docs, not here.

## Branch Protection

All pushes to `main`/`master` require a feature branch + PR — no exceptions,
including "trivial" docs/chore/typo work (which may skip the linked-issue
step but still requires branch + PR + squash-merge). The only bypass is
release tooling (version-bump / lockfile-update commits per the release
workflow), and only for those specific commit types.

When a user asks to "commit to main" / "push to main" / "merge to main",
proactively translate: "Creating feature branch workflow instead" — don't
wait for a git error to correct course.

## Changelog Requirement (Before Merge)

Changelogs go stale when updating them is left to "sometime at release" — so
it is part of every PR, not a separate chore. Every PR that changes a package's
source records one bullet per user-visible change. Docs-only / CI-only PRs may
skip this.

**Prefer a per-PR fragment file.** When the package has a `changelog.d/`
directory, that is where the entry goes:

```
<package>/changelog.d/<issue-or-pr-number>-<short-slug>.md

Fixed   <- line 1: Breaking|Added|Fixed|Performance|Changed|Removed|Security|Documentation

- the bullet text, in the package CHANGELOG's existing style
```

One new file per PR, named after the issue/PR number, means two concurrent PRs
never touch the same lines and git never raises a conflict. A shared
`## [Unreleased]` section does the opposite — it guarantees one. Release time
assembles the fragments into `CHANGELOG.md` and deletes them; never hand-edit
`CHANGELOG.md` in a package that uses fragments.

If the package has no `changelog.d/`, add the bullet to `CHANGELOG.md` under the
topmost `## [Unreleased]` heading (create the heading if the file has none yet),
matching the file's existing style.

- A PR that changes source and lands without a matching changelog entry is a
  **review-gate failure** — the same tier as a failing build/test/lint gate.
  Treat it exactly like CB#8 (QA gate): block the merge, delegate back to the
  Engineer to add the entry, don't wave it through as "trivial."
- If the project also runs automated changelog generation at release time (e.g.
  a conventional-commits generator), check the project's own instructions
  (root `CLAUDE.md`) for which mechanism owns `CHANGELOG.md` before assuming
  they coexist safely. Two writers to one file is a defect; do not invent a
  precedence rule on the fly.

## The trusty-review Gate

Before merge, delegate to the review gate:

```
mcp__trusty-review__review_diff   # in-progress diff review
mcp__trusty-review__review_pr     # open-PR review (correctness/design/security)
```

This is CB#8's evidence for a PR-shaped completion claim — "the PR is ready"
requires a review verdict (or human approval), not just green CI.

## Shipped Defaults: Assignee, Label, and Attribution Footer

These trusty-mpm framework defaults override any harness default and apply to
both the linked issue and the PR:

- **`--assignee @me --label trusty-mpm --label ws/<session-name>`** on every
  `gh issue create` and `gh pr create`. This is multi-harness support: the
  assignee + `trusty-mpm` label identify which issues/PRs a trusty-mpm session
  owns and should pick up; `ws/<session-name>` (this session's own tmux
  session name, via `tmux display-message -p '#{session_name}'`) is how
  per-workstream activity is tracked — labels, never milestones, which stay
  reserved for epics/releases. Full convention + rationale:
  `PM_INSTRUCTIONS.md` "Commits & Issues". Create the labels first if missing:

  ```bash
  gh label create trusty-mpm \
    --description "Created/managed by a trusty-mpm session" --color 8250df \
    2>/dev/null || true
  gh issue create --assignee @me --label trusty-mpm --label "ws/<session-name>" --title "…" --body "…"
  gh pr    create --assignee @me --label trusty-mpm --label "ws/<session-name>" --title "…" --body "…"
  ```

- **Attribution footer** — every commit message and PR body ends with exactly:

  ```
  🤖🤖🤖 Generated with trusty-mpm — https://github.com/bobmatnyc/trusty-tools
  ```

  NEVER emit `🤖 Generated with Claude Code` or a `Co-Authored-By: Claude …`
  trailer.

## Squash-Merge Is Required

PRs merge via **squash-merge only** — one clean commit on `main` per PR.
Delete the feature branch immediately after. No merge commits, no
rebase-merge, for PRs landing on `main`.

```bash
gh pr merge <PR> --squash --delete-branch
```

After a squash-merge the local feature branch will show as "unmerged" to git
(the squashed commit has a different hash) — this is expected, not a sign
the merge failed; see `docs/reference/worktree-discipline.md`.

## Delegation

PR creation itself (branch push, `gh pr create`, description, linking the
issue, requesting reviews) delegates to the **Version Control** agent —
provide it: work summary, files changed, test status, the trusty-review
verdict, and the shipped defaults above (`--assignee @me --label trusty-mpm
--label ws/<session-name>` plus the trusty-mpm attribution footer on the PR
body). The PM constructs the
delegation prompt; it does not run `gh pr create` itself (CB#6 boundary — the
PM stays out of `gh pr`/`gh issue` tooling).

## Related Skills

- `tm-git-file-tracking` — files must be tracked on the feature branch before PR creation
- `tm-circuit-breaker` — CB#6 (forbidden `gh` tool usage) and CB#8 (QA/review gate)
- `tm-ticketing` — the issue-linking half of the same delivery chain
