# Parallel Worktree Discipline — Full Reference

This page expands on the worktree rules mentioned in `CLAUDE.md`. Multiple Claude Code sessions and subagents may share the trusty-tools repo concurrently; this document explains the discipline that keeps them from colliding.

## Context

The main checkout at `/path/to/trusty-tools/` often holds another session's uncommitted work. The workspace implements a **mandatory worktree pattern** to prevent one session from stomping on another's edits, builds, or git state.

## The Main Checkout Is Inspection-Only

From the repo root, the only allowed operations are read-only:
- `git status`, `git log`, `git diff`, `git show`
- File reads (via `Read` tool or `cat`)

**Forbidden in the main checkout's working tree:**
- Edits (`Edit`, `Write`, `sed`, `awk`, `patch`)
- `git reset --hard`, `git checkout .`, `git stash`, `git restore .`
- `cargo build`, `cargo test` (write to `target/`)
- Any command that mutates the working tree, the index, or `target/`

**Rationale:** The working tree is shared state. Any write operation can interfere with other sessions' uncommitted changes, causing unexpected conflicts or data loss.

## All Write-Side Work Happens in a Dedicated Worktree

Provision a new worktree before starting any edit, build, or test:

```bash
git fetch origin main
git worktree add -b <feature-or-fix-branch> \
                  .claude/worktrees/<dirname> origin/main
cd .claude/worktrees/<dirname>
# … edit, build, test, commit, push from here …
```

Each ticket, refactor, or experiment gets its own worktree. Worktrees are disposable — delete them with `git worktree remove --force <path>` once the PR has merged.

**Rationale:** Each worktree is an independent checkout with its own working tree, index, and `target/` directory. Git ensures they do not interfere with each other. One session's build artifacts do not pollute another session's workspace.

## Emergency Operations from the Main Checkout

If you absolutely must run a command from the main checkout — for example `cargo install --path crates/<name> --locked` after a merge — stash first, operate, then restore:

```bash
git -C /path/to/main-checkout stash push -u \
    -m "claude: pre-op-safety $(date +%s)"
# … do the op …
git -C /path/to/main-checkout stash pop
```

Surface the stash name in your report if popping fails so the human can restore manually.

**Rationale:** The stash temporarily shelves uncommitted changes so the working tree is clean for a one-off operation. Restoring afterward brings back the original state. This is a last resort — prefer using a worktree instead.

## Preferred: `cargo install` from a Worktree

The preferred pattern for installing a freshly-built binary onto your PATH is:

```bash
cargo install --path .claude/worktrees/<dirname>/crates/<name> --locked
```

Cargo writes atomically to a temp file and renames into `~/.cargo/bin/`, which keeps the macOS kernel's cdhash cache consistent. The main checkout never needs to be involved.

**Rationale:** `cargo install` from a worktree keeps the main checkout untouched and avoids macOS cdhash cache invalidation issues that can result from manual copies. See the release-workflow note in `CLAUDE.md` for details.

## Subagents Inherit These Rules

Every `Agent`/`Task` dispatch prompt **must**:
- Name the exact worktree path the agent should operate from
- Explicitly forbid leaving that worktree into the main checkout
- Forbid `git reset --hard`, `git checkout .`, and `git stash` against the main checkout
- Forbid touching files outside the assigned worktree

The pattern of instructing an agent to "operate from the main checkout" is banned. QA agents get their own worktree (`.claude/worktrees/qa-<ticket-or-pass>`) just like engineering agents.

**Rationale:** Subagents can modify files and git state. Constraining them to a dedicated worktree prevents cross-session interference and makes cleanup safe (just delete the worktree).

## Worktree Cleanup Is Safe

`git worktree remove --force <path>` deletes the worktree directory but never the main checkout.

After a squash-merge, the local feature branch will appear "unmerged" to git because the squashed commit on `main` has a different hash. Clean up with:

```bash
git branch -D <branch>
git push origin --delete <branch>
```

These operations touch only refs, never working trees.

**Rationale:** Worktrees are fully isolated. Deleting a worktree does not affect the main checkout, other worktrees, or the remote. Git branch cleanup is metadata-only; it affects only the ref database, not the file system.

## Key Takeaways

| Operation | Where | Allowed? |
|---|---|---|
| Read files, git log, git diff | Main checkout | ✅ Yes |
| Edit files, build, test, git write | Main checkout | ❌ No |
| Edit files, build, test, git write | Worktree | ✅ Yes |
| cargo install (one-off) | Main checkout | Only with stash safety |
| cargo install (normal) | Worktree | ✅ Yes (preferred) |
| Delete worktree | Anywhere | ✅ Yes (safe) |
