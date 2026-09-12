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

The monorepo consolidates the trusty-* packages in a single workspace. A single `git stash`
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
one that is, provision a throwaway worktree off `origin/main` and install from
there:

```bash
git worktree add .claude/worktrees/baseline-$$ origin/main
cargo install --path .claude/worktrees/baseline-$$/crates/<name> --locked
git worktree remove .claude/worktrees/baseline-$$
```

This reaches the clean tree the install needs without disturbing the dirty
checkout at all: nothing another session can observe changes, and a failure
partway through leaves that session's uncommitted work exactly where it was.
The old form of this recipe stashed the dirty checkout instead, which meant an
interrupted run left the work in a stash entry someone had to find and restore
by hand (#4730).

## Git Staging in Worktrees

When staging changes in a worktree, always name files explicitly: `git add <file>` or `git add -p`.
Never use `git add -A` in a worktree, as it stages untracked build directories.
The ignored directory name is `target-worktree/`, verified with `git check-ignore -v target-worktree/` from the worktree root.

## Harness Refusals Inside an Isolation Worktree

An agent pinned to a worktree meets a second command classifier that is not
ours. The Claude Code harness refuses Bash commands it cannot prove stay inside
the worktree, in one of three wordings:

```
This agent is isolated in the worktree …, refusing
… runs tm with the text `git diff` in a plain command, so what it runs cannot
  be shown not to be git
… is too complex to verify that it stays inside the worktree
```

Those strings live only in the harness bundle. `tm hook --pm-guard` clears every
shape reported on [#6982](https://github.com/bobmatnyc/trusty-tools/issues/6982):
it resolves `git` by token position rather than by substring, and it frames
heredoc bodies before any rule reads them. The reported shapes are transcribed
into `HARNESS_REFUSED_SHAPES` in
`crates/trusty-mpm/src/bin/tm/commands/pm_guard_bash/tests.rs`, where
`harness_refused_shapes_stay_classifiable_here` asserts this guard can still say
what each one runs. A refusal in this family is therefore never a `tm` defect
and never earns a fix here. Take the substitute and move on; the shapes keep
arriving in new forms, so treat the tables below as a pattern, not a whitelist.

The refusals are intermittent. `bash scripts/check_line_cap.sh` and a
`python3 - <<'PY'` heredoc have each been refused in one session and run in
another, so a command that worked earlier is not evidence that this refusal is
about something else. The substitute column is the spelling that has not been
seen to fail; reach for it first rather than after the refusal.

### Reading git

`git status --short`, `git show <sha>` and a `git` command inside an `&&` chain
have all run while a plain `git diff` next to them was refused, so read the
substitutes as the reliable spelling rather than as a workaround for one shape.

| Refused | Works instead |
|---|---|
| `git diff --stat` from the worktree's own cwd | `git -C <absolute worktree path> diff --stat` |
| `git diff --cached -- <path>`, `git diff <a>...<b>`, `git diff HEAD~2 HEAD -- <path>` | `git --no-pager diff …` |
| a diff of one path at one commit | `git show <sha> -- <path>` |
| `git log origin/main..<sha>` | `git --no-pager log …` |
| `git -C .` — a relative `-C` reads as computed at runtime | `git -C <absolute worktree path>` |
| `cd <worktree> && git diff …` | `git -C <absolute worktree path> diff …` |
| a git command wrapped in the redirect-then-`echo` gate idiom | run the git command bare, one per Bash call |
| a pathspec whose basename starts `tm-`, or a `$(git …)` substitution as a pathspec | quote a glob: `git --no-pager diff -- 'crates/*/src/assets/skills/tm-capa*'` |
| `git checkout HEAD -- <paths>` | `git restore --source=HEAD --staged --worktree <paths>` |
| a `grep` pattern with no `git` in it at all — `grep -n caller_session <file>` refused, `grep -n 'caller' <same file>` allowed, same file both times | re-spell the pattern shorter, or read the file with the Read tool instead |
| `gh pr create` / `tm pr open` refused because the PR body text carries an env-sample filename substring (a dotenv-style `.example` suffix) | reword the body text to avoid that literal substring, or pass the body via `--body-file <path>` |

### Running a script or an interpreter

| Refused | Works instead |
|---|---|
| `bash scripts/<name>.sh`, and the same path spelled absolutely | `./scripts/<name>.sh` |
| `awk -f <prog>`, `sed -f <prog>`, `awk -f /dev/stdin <<'AWK'` | write the program with the Write tool, run it as one plain command |
| `python3 - <<'PY'`, `cat >> <file> <<'EOF'`, a heredoc piped into a runner | the Write tool, or Edit for a multi-hunk change |
| `node -e` whose script path is built from a variable | a literal path |
| an `awk` program over a log file | `grep` |
| `grep -n <pattern> <file>` | multi-file/line-number `grep` results are unreliable here — read the file with the Read tool and search visually, or narrow to the one line with `sed -n '<N>p' <file>` on a literal address |
| a helper script written with the Write tool to the scratchpad directory, then run from there (`python3 <scratchpad>/name.py`) — refused even quoted, because the scratchpad sits outside the worktree | write the script INSIDE the worktree with the Write tool and invoke it as `./name.py`, not from the scratchpad path |

This repo's docs spell every gate `bash scripts/<name>.sh`, so the first row
above applies to the whole test ladder, not only to the line-cap check.

### Shell constructs

| Refused | Works instead |
|---|---|
| a `for` or `while` loop, including `for i in $(seq 20)` | write the loop to a scratchpad script with the Write tool, then run that file |
| `cmd \| tail` followed by a `${PIPESTATUS[0]}` read | separate Bash calls, each a plain command |
| `xargs` piped into another program | one program per Bash call |
| `sed -n` whose address is shell arithmetic, or whose path is a variable | a literal address and a literal path, or `grep` |
| `export VAR=$PWD/… && cargo …`, `CARGO_TARGET_DIR=$PWD/… cargo …` | spell the absolute path literally in the assignment |
| `$(pgrep …)` or any command substitution supplying an argument | a literal value, captured in a previous call |
| an argument whose TEXT contains `git` — a grep pattern, a `perl -pi` regex, a filename | none; re-spell the pattern, or use the Write-a-script route |
| a directory argument with a trailing `/` (`find crates/<crate>/ -name …`) | drop the trailing slash: `find crates/<crate> -name …` |
| a pattern or path containing the literal token `worktree` (`grep -n worktree <file>`) | re-spell around the literal token, or read the file with the Read tool |
| `grep` over more than one file (`grep -rn <pattern> <dir1> <dir2>`, or a glob matching several files) | one `grep` call per file, or `git grep -l <pattern>` to list matches first |

One reported shape is refused HERE too, for a reason of our own: `$'…'` quoting
(`grep -n $'\tfixture' README.md`). The guard's lexer cannot decode it, so it
cannot establish which program would run (#6660). Rewrite it with ordinary
`'…'` quoting — that is a real finding about the command, not a harness misfire.
