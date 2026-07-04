---
name: tm-pr-workflow
description: Branch protection, trusty-review gate, squash-merge, and worktree discipline for landing work on main
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
spec -> issue -> worktree branch -> PR (linked to issue)
     -> trusty-review gate -> squash-merge -> worktree cleanup
```

Every step matters. Skipping the worktree step or the review gate is not a
shortcut — it's a protocol violation the PM must not take on the user's
behalf without being asked.

## Worktree Discipline (Mandatory Before Any Edit)

Per root `CLAUDE.md`: the main checkout is **inspection-only** (read-only
`git status`/`log`/`diff`/`show`, file reads — no edits, no builds or test
runs (whatever the project's gate is — `cargo build`/`test`, `npm run
build`/`test`, `pytest`, ...), no destructive git ops). All write-side work
happens in a dedicated worktree branched off `origin/main`:

```bash
git fetch origin main
git worktree add -b <feature-or-fix-branch> \
                  .claude/worktrees/<dirname> origin/main
cd .claude/worktrees/<dirname>
```

Every subagent dispatch must name the exact worktree path it is confined to
and must never leave it into the main checkout, `git reset --hard`, `git
checkout .`, or `git stash` against main. Clean up after merge with `git
worktree remove --force <path>` and `git branch -D <branch>` — this never
touches the main checkout.

## Branch Protection

All pushes to `main`/`master` require a feature branch + PR — no exceptions,
including "trivial" docs/chore/typo work (which may skip the linked-issue
step but still requires branch + PR + squash-merge). The only bypass is
release tooling (version-bump / lockfile-update commits per the release
workflow), and only for those specific commit types.

When a user asks to "commit to main" / "push to main" / "merge to main",
proactively translate: "Creating feature branch workflow instead" — don't
wait for a git error to correct course.

## The trusty-review Gate

Before merge, delegate to the review gate:

```
mcp__trusty-review__review_diff   # in-progress diff review
mcp__trusty-review__review_pr     # open-PR review (correctness/design/security)
```

This is CB#8's evidence for a PR-shaped completion claim — "the PR is ready"
requires a review verdict (or human approval), not just green CI.

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
provide it: work summary, files changed, test status, and the trusty-review
verdict. The PM constructs the delegation prompt; it does not run `gh pr
create` itself (CB#6 boundary — the PM stays out of `gh pr`/`gh issue`
tooling).

## Related Skills

- `tm-git-file-tracking` — files must be tracked on the feature branch before PR creation
- `tm-circuit-breaker` — CB#6 (forbidden `gh` tool usage) and CB#8 (QA/review gate)
- `tm-ticketing` — the issue-linking half of the same delivery chain
