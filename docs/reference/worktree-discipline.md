# Parallel Worktree Discipline — Extended Reference

Multiple Claude Code sessions and subagents may share this repo concurrently.
The main checkout often holds another session's uncommitted work. To prevent
one session from stomping on another's edits, these rules protect all concurrent
work.

## Why Worktree Discipline Matters

The monorepo consolidates 20 crates in a single workspace. A single `git stash`
can bury another session's work from ANY worktree, not just the main checkout —
the stash stack is repo-global (#4730). A build from the main
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
the main checkout. After a squash-merge the local feature branch will appear
"unmerged" to git because the squashed commit on `main` has a different hash —
use `git branch -D <branch>` and `git push origin --delete <branch>` to clean up.
These operations touch only refs, never working trees.

Deleting a worktree does NOT delete the main checkout or any other worktree.
A worktree is disposable — all durable state lives in git objects and refs.

For the generic discipline (main checkout inspection-only, branch off
`origin/main`, one branch per PR outcome, subagent worktree confinement,
cleanup order), see the "Worktree Discipline" section of the `tm-workflow`
skill — this page carries only what is specific to this repo's Cargo/macOS
toolchain.

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

If a command genuinely needs a clean tree — for example
`cargo install --path crates/<name> --locked` right after a merge lands — use a
throwaway worktree, **never** `git stash`:

```bash
git worktree add /tmp/baseline-$$ origin/main
cargo install --path /tmp/baseline-$$/crates/<name> --locked
git worktree remove /tmp/baseline-$$
```

🔴 **The stash stack is repo-global, not per-worktree.** This page used to
recommend stashing the main checkout before such an op, and that advice caused
three live incidents (#4730): every worktree shares one stack, so a concurrent
agent's `pop` restores and drops the wrong entry while reporting success. The
most recent one restored another session's `trusty-search` WIP, dropped the
popper's own entry, and needed `git fsck --unreachable` to recover the dangling
object.

The throwaway worktree is strictly better than the stash it replaces: it leaves
the main checkout untouched, mutates nothing another session can observe, and
cannot strand someone's work if it fails halfway.
