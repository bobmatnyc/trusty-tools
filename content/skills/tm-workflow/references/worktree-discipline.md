# tm-workflow — worktrees and cleanup

## Worktree Discipline (Mandatory Before Any Source Edit)

The main checkout's write boundary is mechanically enforced, not a convention
to remember (`tm hook --pm-guard`; ADR-0044, ADR-0048). Documents and
configuration stay writable there directly — `.md`, `.toml`, `.json`, `.yaml`,
extension-less files, `.claude/` framework deployment, and `TASK.md` — for the
PM and for every agent it dispatches. Forbidden against it: any SOURCE edit;
`git commit`; any build or test run (whatever the project's gate is: `cargo
build`/`test`, `npm run build`/`test`, `pytest`, …); any destructive git
operation (`git reset --hard`, `git checkout .`, `git stash`,
`git restore .`); any file-mutating command (`sed`/`awk`/`patch`) against a
source file — or anything else that mutates the working tree, index, or build
output outside that documents/configuration carve-out. All source write-side
work happens in a dedicated worktree branched off `origin/main`:

```bash
git fetch origin main
git worktree add -b <feature-or-fix-branch> \
                  .claude/worktrees/<dirname> origin/main
cd .claude/worktrees/<dirname>
```

**Local-only repository — no `origin` remote (#8934).** Branch from the local
default branch instead (`git worktree add -b <branch> .claude/worktrees/<dirname>
main`), skip every fetch, pull, push and PR step, and merge locally: `git merge
--no-ff <branch>` in the main checkout, then `git worktree remove` and `git
branch -d`. This is the one exception to ADR-0061: commits land on local
`main` only in a repository with no remote. Merge only when no sibling agent
is recorded in that checkout — pm-guard denies a HEAD-moving git command there
while the daemon records one. `tm pr` with no `--repo` prints `local-only repo:
no remote; skipping push/PR` and exits 0; with `--repo` it runs and fails. The
session's `gh` is disabled, so no `gh` step can run — except in the
allow-listed Architect directory, which keeps the machine's gh.

**Python editable installs across worktrees (#8386).** pip's wheel cache can
serve one worktree's editable build to another, so the `.pth` file points at a
sibling worktree's source and tests run foreign code. In a Python project, give
each worktree its own venv and install with
`pip install -e <path> --no-cache-dir --force-reinstall`, or skip the install
and run with `PYTHONPATH=<worktree>/src`. Check where the package resolves
(`python -c "import <pkg>; print(<pkg>.__file__)"`) before the first test. Put
this in the brief of any Python dispatch.

🟡 **This is for the PM or a human working directly. It is NOT the dispatch
mechanism** — a subagent dispatch declares `isolation: "worktree"` instead, per
"Worktree Discipline" below and `tm-delegation-patterns`.

Fetch-before-branch, and its fetch-after-merge companion, are BASE-AGENT's Git
Workflow rule — the command block above is that rule's provisioning form. Local
`main` can be stale enough to lose commits, or to leave a fresh branch `BEHIND`
the moment its PR opens.

**Keep the main checkout fresh.** Worktrees branch off `origin/main` and stay
current; nothing refreshes the main checkout, so it drifts — and then every
inspection read (`git log`, opening a file to answer a question, checking
whether a fix already landed) silently answers from old code. At session start,
and after a PR this session merged lands on `origin/main`:

```bash
git -C /path/to/main-checkout fetch origin
git -C /path/to/main-checkout pull --ff-only
```

Fast-forward only — it cannot create a merge commit, cannot rewrite history, and
fails loudly instead of resolving anything silently. A dirty tree does not block
a fast-forward unless the incoming commits touch the same files, so the common
case just works.

**A session pause is one of those merges (#7282).** Where a project tracks
`.trusty-mpm/sessions/`, `/tm-session-pause` no longer commits the snapshot onto
the checkout's current branch — a commit on a PR-only `main` strands there, and
the refresh above then fails on every session. The pause commits onto its own
`chore/sessions-<session>-<ts>` branch off the project's default branch (the one
`config.yaml` declares, else `origin/HEAD`, else `main`), pushes it, and opens an
auto-merging docs-only PR. So run the fetch and `pull --ff-only` above after a
pause PR lands, the same as after any other merge, and the snapshot is visible
locally again. Where a project gitignores that store instead, a pause opens no
PR and owes no refresh — the snapshot is machine-local and stays on disk.

🟡 **One cause is an untracked path the incoming commits also add.** `--ff-only`
then aborts with "The following untracked working tree files would be
overwritten by merge", naming the path and nothing else. Diff that untracked
copy against the incoming one BEFORE pulling rather than improvising a recovery
after the abort: identical content resolves itself, differing content is
reported with the diff. The check and both outcomes are in
`Skill(skill="git-workflow")`, under the heading
"Pulling After a Merge — Check Untracked Collisions First" (#7558).

🔴 **If `--ff-only` fails, never clear the way by discarding.** The other usual
cause is another session's uncommitted work. Do not `git stash` — repo-level and
shared across every worktree, so it yanks state out from under sessions running
right now. Do not `git checkout --`, `restore`, `reset --hard`, or `clean`: the
main-checkout guard blocks these, correctly, and hunting for an unblocked
equivalent is routing around a safety control. A file showing ` M` in
`git status --porcelain` was never staged, so nothing recovers it once
discarded.

Identify the owner first — `git status --porcelain` for staged-vs-unstaged, file
mtimes, and the session log for who was alive in that window — and read what a
live peer is doing, per "Concurrent Sessions on One Repo" (`concurrent-sessions-and-merge-queue.md`), before assuming
the work is abandoned. If it is unowned, **preserve rather than discard**: commit
it out of the way, which is permitted and destroys nothing.

```bash
git checkout -b orphan/main-checkout-$(date +%Y%m%d)
git add -u && git commit -m "wip: orphaned main-checkout changes"
git checkout main && git pull --ff-only
```

Committing reaches the same clean tree as discarding, costs the same number of
steps, and cannot lose anything — the guard exists to prevent loss, not to
prevent refreshing, so the compliant path and the safe path are the same path.
If the checkout has genuinely diverged instead, report it and stop.

This is inspection hygiene only, so reads are not answered from stale code. The
main checkout stays read-only for source; every source edit still happens in a
worktree, and the boundary is enforced mechanically rather than left to
discipline alone. `git pull` is already on the PM's allowlist — no new
authority, not a budgeted direct action.

**A worktree is a writer; the branch is the workstream.** The durable unit is
the branch — one branch per workstream, one session per workstream. A worktree
is only the checkout that lets you write to that branch: ephemeral, disposable,
recreatable at any time with `git worktree add`. Losing a worktree loses nothing
the branch does not still hold.

**One branch and worktree per independently reviewable PR outcome** — not per
ticket, per refactor step, or per experiment. Several related tickets may share
one worktree when a single coherent change satisfies them (`Refs #A`,
`Refs #B`); see "One Outcome, One PR" in `SKILL.md` for everything that outcome owes
and keeps bundled in that same worktree and PR.

**Experiments stay session-local.** Promote an experiment to a branch and
worktree only once its result is accepted for implementation.

🔴 **A file-mutating subagent dispatch declares `isolation: "worktree"`. That is
the only sanctioned mechanism, and the PM never tells the agent to hand-roll one
— not a literal `git worktree add`, and not prose like "work in a worktree of
your own" (#5649).** The harness provisions the tree and puts the agent in it. A
hand-rolled worktree is invisible to `tm hook --pm-guard`, which reads the
declared `isolation` parameter and never the prompt — so an agent that made its
own tree still counts as occupying the shared HEAD, and the next file-mutating
dispatch is denied for a collision that does not exist. The prose form recurred
in a different project on 2026-09-09, after the literal-command form was fixed
here — the guard cannot see either phrasing, so both are banned, regardless of
wording.

**When `isolation` is unavailable, the PM serializes.** Dispatch one
file-mutating agent, wait for it, dispatch the next. Serializing is always
available and always correct. Hand-rolling a worktree in order to parallelize
anyway is what this rule forbids.

**A brief that needs a named branch names the BRANCH, never a second worktree
(#8337).** An isolated agent that runs `git worktree add` for a ticket-named
path gets the tree, then every Edit, `git -C`, `--git-dir` and removal against
it is refused: Claude Code pins the agent to its assigned worktree, and no
`tm` hook can move that pin. The sanctioned pattern: the agent stays in its
assigned worktree and creates the target-convention local branch there,
tracking the remote branch (`git checkout -b DE-2854 --track origin/DE-2854`),
then pushes it. When the repository needs the directory itself to carry the
ticket name, the PM creates that worktree before dispatch and serializes the
work into it; the agent never creates it.

The dispatch still forbids leaving the assigned tree into the main checkout, and
forbids `git reset --hard`, `git checkout .`, and `git stash` against main.

**Who counts as file-mutating** (#5650): every engineer-tier agent, plus
`documentation` and the three QA agents that author tests — `qa`, `web-qa`,
`api-qa`. Each gets its own worktree, same as an engineer. A dispatch that only
reads does not: `research`, `code-analyzer`, `ticketing`, and `code-critic`,
which shares `role: qa` with the QA writers but recommends rather than edits.
The guard reads this from the bundled agent's own frontmatter
(`dispatch_isolation.rs`), so a custom or project-local agent it does not ship is
never denied — declare `isolation: "worktree"` for one that writes.

🔴 **`version-control` is the one writer that keeps the checkout it is given
(ADR-0056, owner ruling 2026-08-31).** It merges into main and reclaims merged
worktrees, and neither can be done from inside a worktree — isolating it removes
the tree the work needs. The guard therefore neither diverts it in a main
checkout nor denies it beside another writer. It still counts as an OCCUPANT: an
engineer dispatched into the same directory beside it is denied exactly as
before, and engineer source-write confinement is untouched.

🔴 **Cleanup after merge belongs to `version-control` or the PM, never to the
agent that did the work (#5791, ADR-0056).** An engineer reports the merged PR,
the worktree path, and the branch, then stops. `version-control` — or the PM —
confirms `state: MERGED` per PR and reclaims the trees:

```bash
tm session prune-worktrees --merged-prs            # preview, the default
tm session prune-worktrees --merged-prs --force    # reclaim
```

That pass spares any worktree still holding unsaved work, still claimed by a
managed session, or still owned by a live agent, and it reports each one it
spared with the reason. It removes the checkout only — the local branch stays,
and the remote branch is deleted by `tm pr cleanup <n>`; `--delete-branch`
leaves it behind when a worktree holds the head (#8391).
`tm hook --pm-guard` denies an agent-side `git worktree remove` and names this
command as the remedy, so an agent that reaches for it gets redirected rather
than silently blocked. BASE-AGENT's Git Workflow section states the agent's half.

Two guards made this undelegable to any agent, and ADR-0056 lifted both for
`version-control` alone: its unisolated dispatch is no longer denied as a second
writer on the shared HEAD (#4480), and it is no longer diverted into a worktree
whose fences would keep it off the shared registry. Every other agent is still
held by both, and `git worktree remove` is still denied to all of them — the
prune pass above is the cleanup path precisely because it spares a tree that
still holds work.

**Escape hatch — a throwaway worktree.** If you genuinely need a clean tree to
run one command — a baseline check, a bisect, a build against `origin/main` —
provision a disposable one rather than clearing a checkout another session may
be writing:

```bash
git worktree add .claude/worktrees/baseline-$$ origin/main
# … do the op …
git worktree remove .claude/worktrees/baseline-$$
```

This escape hatch used to stash the main checkout, operate, then restore. The
throwaway worktree needs no main checkout at all, and a run that dies partway
through leaves every other tree exactly as it was instead of stranding
uncommitted work in a stash entry a human has to go find (#4730).

It is the PM's hatch, not an agent's. An agent does not create its own worktree
(#5649), and the guard denies it the `remove` line outright (#5791). An agent
that needs a clean tree asks the PM for one.

Project-specific worktree hazards (binary-install caveats, code-signing caches,
and the like) belong in the project's own reference docs, not here.

## Task-Owned Cleanup

Inventory only the paths this task owns; preserve other sessions and dirty or
unpushed work. Confirm the merged PR and live ownership before removal. Route
the operation through the authorized `version-control` agent. Use literal
paths with the supported guard, not shell-variable loops, for example:
`git -C /absolute/repo worktree remove /absolute/repo/.claude/worktrees/task-name`.
The paths are placeholders to replace with verified targets, never a glob.
A refusal is a finding to resolve, not permission to use `rm -rf` or global
`tm sessions prune-worktrees --merged-prs --force`. See #8021.
