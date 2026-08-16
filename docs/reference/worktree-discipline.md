# Parallel Worktree Discipline — Extended Reference

Multiple Claude Code sessions and subagents sharing this repo concurrently is
the normal, intended arrangement, not a hazard to work around. The main
checkout often holds another session's uncommitted work.

**The write boundary that protects that work is mechanically enforced, not
left to convention** (`tm hook --pm-guard`; [ADR-0044](../adr/0044-main-checkout-write-boundary-and-agent-worktree-ownership.md),
[ADR-0048](../adr/0048-dispatched-writers-get-a-worktree-and-the-write-boundary-is-enforced.md),
[ADR-0049](../adr/0049-docs-commits-are-permitted-in-a-main-checkout.md)).
Documents and configuration — `.md`, `.toml`, `.json`, `.yaml`,
extension-less files, `.claude/` framework deployment, `TASK.md` — stay
writable directly in the main checkout for the PM and every agent it
dispatches; source edits there are denied for both. `git commit` there is
denied too, except for a staged set that is entirely documents and
configuration (ADR-0049) and only when no other session is writing the same
checkout. A dispatched agent that may write is granted its own worktree under
`.claude/worktrees/` automatically the moment the session is standing in a
main checkout — see [ADR-0036](../adr/0036-all-worktrees-are-siblings-under-claude-worktrees.md)
for where that worktree lives. The rules below are what remains a matter of
discipline rather than mechanical enforcement.

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

Install from a checkout with an empty `git status --porcelain`, at a known
commit — `cargo install --path` bakes in whatever is actually on disk, so a
dirty or unverified checkout ships whatever it happens to be holding:

```bash
git -C <checkout> status --porcelain   # must print nothing
git -C <checkout> log -1 --oneline     # is this the commit you meant to ship?
cargo install --path <checkout>/crates/<name> --locked
```

Cargo writes atomically to a temp file and renames into `~/.cargo/bin/`,
which keeps the macOS kernel's cdhash cache consistent — see
[release-workflow.md](release-workflow.md) for the full cdhash hazard (why a
bare `cp` over an on-PATH binary SIGKILLs the next exec, and the TCC-grant
consequences). That property holds for `cargo install --path` from any clean
checkout; it says nothing about which checkout to pick.

A freshly-provisioned worktree off `origin/main` satisfies the clean-tree
requirement by construction, which is why it stays the default:

```bash
cargo install --path .claude/worktrees/<dirname>/crates/<name> --locked
```

The main checkout is not automatically disqualified, but it is not
automatically clean either. The write boundary
([ADR-0044](../adr/0044-main-checkout-write-boundary-and-agent-worktree-ownership.md),
[ADR-0048](../adr/0048-dispatched-writers-get-a-worktree-and-the-write-boundary-is-enforced.md),
[ADR-0049](../adr/0049-docs-commits-are-permitted-in-a-main-checkout.md)) denies
source edits and source commits there, which rules out the worst case, but it
classifies by file EXTENSION, not by directory: documents and configuration
stay writable, and since ADR-0049 committable, directly in the main checkout.
`crates/trusty-mpm/src/assets/skills/*.md` falls on the writable side of that
line even though it lives under `src/` and is compiled into the `trusty-mpm`
binary at build time via `include_str!` — so a locally-edited, uncommitted (or
locally-committed-but-unpushed) skill file in the main checkout can still be
baked into a binary installed from there. The `git status --porcelain` check
above is what catches that; running it costs one command.

If the checkout you need to install from is not clean and you cannot switch to
one that is, stash first, operate, then restore:

```bash
git -C /path/to/checkout stash push -u \
    -m "claude: pre-op-safety $(date +%s)"
# … confirm `git status --porcelain` is now empty, then install …
git -C /path/to/checkout stash pop
```

Surface the stash name in the report if popping fails, so the change can be
restored manually. This is the narrow exception the `tm-workflow` worktree
rules call out — it does not license routine edits from the main checkout.
