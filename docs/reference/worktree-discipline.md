# Parallel Worktree Discipline — Extended Reference

Multiple Claude Code sessions and subagents may share this repo concurrently.
The main checkout often holds another session's uncommitted work. To prevent
one session from stomping on another's edits, these rules protect all concurrent
work.

## Why Worktree Discipline Matters

The monorepo consolidates 20 crates in a single workspace. A single `git stash`
from the main checkout can bury another session's work. A build from the main
checkout writes to the shared `target/` tree and fills the filesystem with
build artifacts that interfere with other sessions. A `git reset --hard` from
the main checkout can permanently lose uncommitted changes that another session
depends on.

Worktrees isolate each session into its own branch and filesystem tree. Each
worktree has its own index, staging area, and workspace. Commits and branches
in one worktree are fully orthogonal to commits in the main checkout or other
worktrees — they share only the object database and refs.

## Worktree Cleanup

`git worktree remove --force <path>` deletes the worktree directory but never
the main checkout — deleting a worktree does NOT delete the main checkout or
any other worktree. A worktree is disposable; all durable state lives in git
objects and refs.

What is specific to this repo: every merge here is a squash merge, so the
local feature branch's tip commit is routinely NOT an ancestor of the squashed
commit that lands on `main` — same content, different hash. `git branch -d`
sees that as "not fully merged" and refuses even when the PR merged cleanly —
a plain `git pull --ff-only` on a stale local checkout does not fix this, it
only compounds it, since the ancestry check runs against whatever `main` that
checkout currently has. Treat git's own ancestry check as untrustworthy here;
`gh pr view <branch> --json state` is the real signal.

For the operational rule (worktree-then-branch order, confirming the merge via
`gh pr view` before force-deleting the branch, and never deleting a branch
that was never pushed and holds unique commits), see BASE-AGENT's Git Workflow
section, cross-referenced from the "Worktree Discipline" section of the
`tm-workflow` skill.

## Installing a Freshly Built Binary

Prefer installing from the worktree, never the main checkout:

```bash
cargo install --path .claude/worktrees/<dirname>/crates/<name> --locked
```

Cargo writes atomically to a temp file and renames into `~/.cargo/bin/`,
which keeps the macOS kernel's cdhash cache consistent — see
[release-workflow.md](release-workflow.md) for the full cdhash hazard (why a
bare `cp` over an on-PATH binary SIGKILLs the next exec, and the TCC-grant
consequences). The main checkout never needs to be involved.

If a command genuinely must run from the main checkout — for example
`cargo install --path crates/<name> --locked` right after a merge lands —
stash first, operate, then restore:

```bash
git -C /path/to/main-checkout stash push -u \
    -m "claude: pre-op-safety $(date +%s)"
# … do the op …
git -C /path/to/main-checkout stash pop
```

Surface the stash name in the report if popping fails, so the change can be
restored manually. This is the narrow exception the `tm-workflow` worktree
rules call out — it does not license routine edits from the main checkout.
