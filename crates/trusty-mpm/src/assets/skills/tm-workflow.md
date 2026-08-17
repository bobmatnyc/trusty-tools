---
name: tm-workflow
description: The single trusty-mpm delivery workflow — phases and gates, the ticketing/workflow/version-control ownership boundary and handoff, worktree and branch discipline, changelog, PR body, review gate, squash-merge, cleanup, and how a project customizes the workflow via CLAUDE.md
user-invocable: true
version: "2.0.0"
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
| The `version-control` agent | Branch/worktree/commit/push mechanics and the whole PR lifecycle: create, title/body, issue-closing links, reviewers, checks, update-branch, merge, branch cleanup | Authoring workflow policy, deciding whether an issue is warranted, changing issue metadata or state |

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
   writes the PR body and inserts the correct `Closes owner/repo#N` (or a
   non-closing relationship link when the PR does not finish the issue).
3. **After merge, PM → `ticketing`**: the merged PR number and squash SHA, for
   the completion comment or the close.

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
- **One PR may close several tickets** when one coherent change satisfies them
  (`Closes #A`, `Closes #B`). Prefer that over several coupled PRs with an
  artificial merge order.
- Split only when the outcomes can be reviewed, deployed, or reverted
  independently, or when risk or size makes a stack materially safer to review.

## The 5-Phase Model and Its Dispatch Briefs

The instruction package's CORE phase table is canonical for **whether** a phase
runs and carries each skip condition. Where a phase runs, its gate is blocking —
"conditional" governs entry, never rigour (#4594). A project may replace the
whole workflow section via a `WORKFLOW` marker in its `CLAUDE.md` (see
"Customizing the Workflow" below) if its delivery process differs.

**Phase 1 — Research** (`research`). Required for ambiguous requirements,
multiple possible approaches, or an unfamiliar codebase. Skipped when the user
gave an explicit command or the task is simple operational work.

```
Task: Analyze requirements for [feature]
Return: Technical requirements, gaps, measurable criteria, approach
```

**Phase 2 — Code Analysis** (`code-analyzer`, sonnet — NOT `code-critic`, which
is a separate agent).

```
Task: Review proposed solution
Use: think/deepthink for analysis
Return: Approval status with specific recommendations
```

Decision: APPROVED → Implementation. NEEDS_IMPROVEMENT → back to Research.
BLOCKED → escalate to the user.

**Phase 3 — Implementation** (the language-specific engineer where one exists).
Requirements: complete code, error handling, test proof, and the changelog entry
described below. Skip only for docs-only/CI-only changes.

**Phase 4 — QA.** Routing: `api-qa` for APIs, `web-qa` for UI, `qa` otherwise.
The gate itself is `tm-verification-protocols`.

**Phase 5 — Documentation** (`documentation`). Skipped for an internal refactor
with no public API change.

### Override Commands

| User says | Effect |
|---|---|
| "Skip workflow" | Bypass the phase sequence |
| "Go directly to [phase]" | Jump to that phase |
| "No QA needed" | Skip phase 4 (not recommended) |
| "Emergency fix" | Bypass Research |

Honour the override and name the bypassed gate in the completion report, so the
missing evidence is visible rather than implied.

### Verification Gates Are Not an Override Target

The verification-gate contract in `tm-verification-protocols` is a
project-independent invariant enforced by CB#8. A custom `WORKFLOW` override can
change *when* QA runs, never *whether* a completion claim requires evidence.

## Sprint, then Harden — the Rest of the Doctrine

The instruction package states the two phases, where to spend the verification
budget, and the hard line (never turn red green by deleting coverage). Two
derived rules live here because they apply only at a specific moment:

- **A branch that has drawn 3+ review rounds is evidence to close and fold**, not
  to attempt round 4. Worked example: #4202 → #4207.
- **Branch = workstream, and it is durable. Worktree = writer, and it is
  ephemeral.** Keep worktrees short-lived; keep branches workstream-scoped.

Slow feature release *causes* too many things in flight. Shortening time-to-land
is the fix; capping WIP treats the symptom.

## Test Scope Widens by Stage

Unit tests run on the new or changed code only while developing, on the full
test files that changed when merging, and on the full corpus only when
publishing.

| Stage | Scope |
|---|---|
| Developing | tests covering the new or changed code only |
| Merging | the full test files that changed |
| Publishing | the full corpus |

- **Developing** is the inner loop: the targeted test that proves the change,
  re-run as you edit. Nothing wider is owed while the code is still moving.
- **Merging** widens to whole files, never to the whole repository. Every test
  file the diff touched runs in full — including the cases you did not edit, and
  any normally-skipped test that lives in one of those files. A change to a
  public interface or to shared test infrastructure alters what a dependent
  package's test files mean, so those files count as changed even though the
  diff never opened them.
- **Publishing** is the only stage that owes the whole corpus, plus whatever
  release gates the project defines.

This is the two-phase doctrine at finer grain: developing is SPRINT; merging and
publishing are two different widths inside HARDEN, and merging is the narrower.
A green merge gate is therefore not a release gate.

Stage and rigour are separate axes and both bind. The stage decides how *wide* a
gate runs; the project's risk labels and test ladder decide how *hard* it is
applied and which gates run at all. Pick the rung from the project's `CLAUDE.md`,
then read the stage off where you are in the delivery chain.

A consequence for branch protection: a full-corpus CI job is a publish gate, so a
project may leave it off the required pre-merge contexts and merge while it is
still pending. A **failing** check blocks the merge at every stage — only
*pending* is tolerated.

🔴 **Widening by stage is never licence to skip.** Choosing the narrower scope is
a claim about blast radius that you must be able to prove. It is never licence to
make a red gate green by deleting a test, marking it skipped or ignored, gating
it out of the build, or excluding it from the run. That hard line ("Sprint, then
Harden") is unchanged.

## Worktree Discipline (Mandatory Before Any Edit)

The main checkout is **inspection-only** — read-only `git status`/`log`/`diff`/
`show` and file reads. Forbidden against it: any edit; any build or test run
(whatever the project's gate is: `cargo build`/`test`, `npm run build`/`test`,
`pytest`, …); any destructive git operation (`git reset --hard`,
`git checkout .`, `git stash`, `git restore .`); any file-mutating command
(`sed`/`awk`/`patch`) — or anything else that mutates the working tree, index,
or build output. All write-side work happens in a dedicated worktree branched
off `origin/main`:

```bash
git fetch origin main
git worktree add -b <feature-or-fix-branch> \
                  .claude/worktrees/<dirname> origin/main
cd .claude/worktrees/<dirname>
```

Always `git fetch origin main` first and branch off `origin/main`, never local
`main` — local `main` can be stale and branching from it has caused lost commits.

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

🔴 **If `--ff-only` fails, never clear the way by discarding.** The usual cause
is another session's uncommitted work. Do not `git stash` — repo-level and
shared across every worktree, so it yanks state out from under sessions running
right now. Do not `git checkout --`, `restore`, `reset --hard`, or `clean`: the
main-checkout guard blocks these, correctly, and hunting for an unblocked
equivalent is routing around a safety control. A file showing ` M` in
`git status --porcelain` was never staged, so nothing recovers it once
discarded.

Identify the owner first — `git status --porcelain` for staged-vs-unstaged, file
mtimes, and the session log for who was alive in that window — and ask live
peers before assuming the work is abandoned. If it is unowned, **preserve rather
than discard**: commit it out of the way, which is permitted and destroys
nothing.

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
main checkout stays read-only for work; every edit still happens in a worktree.
`git pull` is already on the PM's allowlist — no new authority, not a budgeted
direct action.

**A worktree is a writer; the branch is the workstream.** The durable unit is
the branch — one branch per workstream, one session per workstream. A worktree
is only the checkout that lets you write to that branch: ephemeral, disposable,
recreatable at any time with `git worktree add`. Losing a worktree loses nothing
the branch does not still hold.

**One branch and worktree per independently reviewable PR outcome** — not per
ticket, per refactor step, or per experiment. Several related tickets may share
one worktree when a single coherent change satisfies them (`Closes #A`,
`Closes #B`); see "One Outcome, One PR" above for everything that outcome owes
and keeps bundled in that same worktree and PR.

**Experiments stay session-local.** Promote an experiment to a branch and
worktree only once its result is accepted for implementation.

Every subagent dispatch must name the exact worktree path it is confined to, and
must forbid leaving it into the main checkout, `git reset --hard` and
`git checkout .` against main, and `git stash` ANYWHERE — the stash stack is
repo-global, so it is the one prohibition that does not narrow to the main
checkout (#4730). QA agents get their own worktree (e.g.
`.claude/worktrees/qa-<ticket-or-pass>`), same as engineering agents.

Clean up after merge with `git worktree remove --force <path>` (which deletes the
worktree directory and never the main checkout), then `git branch -D <branch>`
and, when the squash-merge did not already do it, `git push origin --delete
<branch>` — the branch goes last, because until the squash-merge lands it is the
only durable copy of the workstream.

🔴 **Never `git stash` — anywhere in the repo, not just against main.** The
stash stack is repo-global, not per-worktree: every worktree shares one stack,
so a concurrent agent's `pop` can restore and drop YOUR work, and `pop` reports
success either way. Three live incidents (#4730) — the most recent restored
another session's WIP and dropped the popper's own entry, recovered only via
`git fsck --unreachable`.

**Escape hatch — a throwaway worktree, never a stash.** If you genuinely need a
clean tree to run one command — a baseline check, a bisect, comparing against
`origin/main` — provision one instead of disturbing an existing checkout:

```bash
git worktree add /tmp/baseline-$$ origin/main
# … run the check there …
git worktree remove /tmp/baseline-$$
```

This is strictly better than the stash it replaces: it needs no main checkout,
mutates nothing another session can see, and cannot fail halfway and strand
someone's work.

If you truly cannot avoid stashing: label it (`git stash push -m "<purpose>"`),
never `pop` blind — `git stash list` first, pop BY REF — and verify the restored
files are the ones you stashed. Surface the stash ref in your report either way,
so a human can recover it manually.

Project-specific worktree hazards (binary-install caveats, code-signing caches,
and the like) belong in the project's own reference docs, not here.

## Git Security Review (Mandatory Before Push)

Before any `git push`, delegate a credential scan to the `security` agent:

1. `git diff origin/main...HEAD` — the diff about to be pushed. Three-dot,
   because it diffs from the merge base and shows only what YOUR branch changed.
   Two-dot compares the two commits, so files DELETED from `main` since your
   branch point come back as your additions: a measured run reported 19 hits
   that were another PR's deletions where three-dot reported zero across 36
   files. A scan people learn to wave through is where a real secret hides.
2. `security` scans it for API keys, passwords, private keys, and tokens, and
   returns either clean or the list of blocked items.
3. **Block the push if secrets are detected.** A leaked credential in git history
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

## Changelog Requirement (Before Merge)

Changelogs go stale when updating them is left to "sometime at release", so it is
part of every PR. Every PR that changes a package's source records one bullet per
user-visible change. Docs-only / CI-only PRs may skip this.

**Prefer a per-PR fragment file.** When the package has a `changelog.d/`
directory, that is where the entry goes:

```
<package>/changelog.d/<issue-or-pr-number>-<short-slug>.md

Fixed   <- line 1: Breaking|Added|Fixed|Performance|Changed|Removed|Security|Documentation

- the bullet text, in the package CHANGELOG's existing style
```

One new file per PR, named after the issue/PR number, means two concurrent PRs
never touch the same lines. A shared `## [Unreleased]` section guarantees a
conflict instead. Release time assembles the fragments into `CHANGELOG.md` and
deletes them; never hand-edit `CHANGELOG.md` in a package that uses fragments.

If the package has no `changelog.d/`, add the bullet to `CHANGELOG.md` under the
topmost `## [Unreleased]` heading (create it if absent), matching the file's
existing style.

- A PR that changes source and lands without a matching changelog entry is a
  **review-gate failure** — the same tier as a failing build/test/lint gate.
  Treat it exactly like CB#8: block the merge, delegate back to the engineer,
  don't wave it through as "trivial."
- If the project also runs automated changelog generation at release time, check
  the project's root `CLAUDE.md` for which mechanism owns `CHANGELOG.md` before
  assuming they coexist. Two writers to one file is a defect; do not invent a
  precedence rule on the fly.

## Minimal PR Body (seven fields)

The `version-control` agent writes this; the PM supplies the material.

1. Primary outcome and linked issue(s), with the `Closes owner/repo#N` link.
2. What changed, and what is intentionally out of scope.
3. Risk / blast radius.
4. Test evidence at the applicable levels.
5. Baseline/pre-existing failures and their canonical issue (see below).
6. Documentation/changelog status.
7. Review-finding disposition: fixed here, kept on the parent, or separately
   ticketed.

**PR body freshness**: when scope or claims change mid-flight, delegate the
update to `version-control` immediately rather than leaving stale assertions.

## Baseline-Failure Protocol (Red That Isn't Yours)

When a required gate goes red, establish whose red it is before doing anything
else.

1. Never turn red green by removing coverage. That hard line is stated in the
   instruction package ("Sprint, then Harden") and is the entry condition for the
   rest of this protocol.
2. Determine whether the branch caused the failure: run the same gate on the base
   branch, check the failing test's recent history, or reproduce it in isolation.
3. **Branch-caused** → fix it in this PR.
4. **Pre-existing** → the disposition is `tm-ticketing`'s to decide (`COMMENT`,
   `REOPEN`, `NEW REGRESSION`, or `NO TICKET`). Hand the run URL, SHA, command,
   and failure signature to `ticketing` and let its gate choose; a single
   unrelated red run is an observation, not automatically a ticket.
5. Report the gate result in exactly this shape:

   ```
   change-specific gates pass; <gate name> blocked by canonical issue #N
   ```

   Never report "all tests pass" while a gate is red, whoever caused it. Merge
   disposition then follows branch protection and the risk tier; a red required
   check is never merged around.

## The trusty-review Gate

Before merge, delegate to the review gate:

```
mcp__trusty-review__review_diff   # in-progress diff review
mcp__trusty-review__review_pr     # open-PR review (correctness/design/security)
```

This is CB#8's evidence for a PR-shaped completion claim — "the PR is ready"
requires a review verdict (or human approval), not just green CI.

## Squash-Merge Is Required

PRs merge via **squash-merge only** — one clean commit on `main` per PR. Delete
the feature branch immediately after. No merge commits, no rebase-merge, for PRs
landing on `main`.

```bash
gh pr merge <PR> --squash --delete-branch
```

After a squash-merge the local feature branch shows as "unmerged" to git (the
squashed commit has a different hash). That is expected, not a failed merge.

## Shipped Defaults on the PR

These trusty-mpm framework defaults override any harness default and belong in
the `version-control` delegation brief:

- **`--assignee @me --label trusty-mpm --label ws/<session-name>`** on every
  `gh pr create`. The assignee + `trusty-mpm` label identify which PRs a
  trusty-mpm session owns; `ws/<session-name>` (this session's tmux session name,
  via `tmux display-message -p '#{session_name}'`) tracks per-workstream
  activity. A workstream is a **label**, never a milestone.

  ```bash
  gh label create trusty-mpm \
    --description "Created/managed by a trusty-mpm session" --color 8250df \
    2>/dev/null || true
  gh pr create --assignee @me --label trusty-mpm --label "ws/<session-name>" \
    --title "…" --body "…"
  ```

  The equivalent issue-side defaults, and the type/component/priority label
  families that stack on them, are `tm-ticketing`'s — do not restate them in a
  version-control brief.

- **Attribution footer** — every commit message and PR body ends with exactly:

  ```
  🤖🤖🤖 Generated with trusty-mpm — https://github.com/bobmatnyc/trusty-tools
  ```

  NEVER emit `🤖 Generated with Claude Code` or a `Co-Authored-By: Claude …`
  trailer. This is a Framework-Guaranteed Convention stated in the instruction
  package; it is repeated here because it must appear verbatim in the delegation
  brief.

## Delegating the PR

PR creation and every later PR edit go to `version-control` — branch push,
`gh pr create`, the body, the issue-closing link, reviewers, `gh pr checks`, and
the merge. Give it: work summary, files changed, test status, the trusty-review
verdict, the canonical issue context from `ticketing`, and the shipped defaults
above. The PM constructs the brief; it never runs `gh pr` itself (P7 / CB#6).

## Customizing the Workflow

trusty-mpm has no separate "workflow engine" a project configures at runtime. The
PM's phase/gate behavior comes from the bundled instruction package, and a
project customizes it via named-section marker blocks in its root `CLAUDE.md`.
This is implemented in `core/instruction_overrides.rs`,
`core/instruction_pipeline.rs`, and `core/claude_md_sections.rs`.

### How the PM Prompt Is Assembled

The bundled PM prompt has one source of truth: the JSON manifest
`assets/instructions/pm-instruction-package.json` (schema v2), embedded at
compile time via `bundled_pm_package.rs`. It declares section order and
composition; the prose for each section is stored separately in
`assets/instructions/sections/*.md`, pulled in as `include_str!` constants
registered in the `SECTION_SOURCES` table (`core/instruction_pipeline.rs`) — a
missing section file is a compile error, not a launch-time surprise. The nine
marker tokens (`core/claude_md_sections.rs::section_token`) are `IDENTITY`,
`CORE`, `MEMORY`, `SEARCH`, `WORKFLOW`, `AGENT-DELEGATION`, `ENFORCEMENT`,
`NON-OVERRIDABLE-RULES`, and `FRAMEWORK-GUARANTEED-CONVENTIONS`.

**`CORE` is the only one a project cannot replace.** Every other section,
including `NON-OVERRIDABLE-RULES` and `FRAMEWORK-GUARANTEED-CONVENTIONS`, can be
overridden — there is no separate "floor" concept anymore (the
`is_floor()`/`instruction_floor.sha256` machinery was retired by #4286, being the
appearance of a control rather than one a project-owned `CLAUDE.md` could
enforce).

At session start, `core/instruction_overrides.rs::resolve_pm_prompt` (reached via
`build_system_prompt_for*`) composes the final prompt. It is not a file a user
edits — it is composed fresh per launch.

### The One Customization Surface

A project customizes any non-`CORE` section exactly one way: a named-section
marker, `<!-- TRUSTY-MPM: <TOKEN> START v=1 -->` … `<!-- TRUSTY-MPM: <TOKEN> END
-->`, in the project's root `CLAUDE.md` — the sole marker host
(`core/claude_md_sections.rs::HOST_FILES`). This replaces exactly the matching
section, nothing else.

The five legacy per-file overrides
(`.trusty-mpm/{INSTRUCTIONS,AGENT_DELEGATION,WORKFLOW,MEMORY,
PM_INSTRUCTIONS_DEPLOYED}.md`) are RETIRED (#4286) and never read. Never create
one; if a project still has one, move its contents into `CLAUDE.md` and delete
the file — `tm doctor`'s `legacy_overrides` check fails until it is gone.

`CLAUDE.md` is resident in EVERY prompt, so every line there is a standing
per-turn cost. Content needed on every prompt belongs there; content needed only
sometimes belongs in a skill, a doc, or memory.

Robustness: a missing `CLAUDE.md`, a missing marker block, or an empty marker
body all fall back silently to the bundled default — a customization attempt
never blanks a section or crashes launch.

The agent-delegation roster is DYNAMIC, not authored prose: it comes from
`deployed_roster_section` → `roster_from_dirs`, a union of the project tier,
`$CLAUDE_CONFIG_DIR/agents`, and `~/.claude/agents`, rendered by
`generate_authority`. It is non-droppable — `validate_roster` rejects a package
where the roster generator is optional or absent (#4069).

### Trigger Phrases

| User says | PM writes to |
|---|---|
| "remember/always/never/for this project" | Plain prose in `CLAUDE.md` (no marker needed) |
| "use X agent for Y" / "route/change agent" | `<!-- TRUSTY-MPM: AGENT-DELEGATION START v=1 -->` block in `CLAUDE.md` |
| "add/change workflow phase" | `<!-- TRUSTY-MPM: WORKFLOW START v=1 -->` block in `CLAUDE.md` |
| "memory behavior" | `<!-- TRUSTY-MPM: MEMORY START v=1 -->` block in `CLAUDE.md` |

After writing an override, confirm the marker to the user and note it "takes
effect at next session startup" — the resolved prompt is assembled at
session-prepare time, not hot-reloaded.

### Inspecting the Resolved Prompt

```bash
tm sessions instructions       # prints the resolved prompt on stdout
cat .trusty-mpm/last-instructions.md   # the exact stash resolve_pm_prompt wrote
```

`tm sessions instructions` reports every applied, declined, and shadowed marker
on **stderr**, so `tm sessions instructions >/dev/null` alone answers "why didn't
my override apply?". `last-instructions.md` is written by `prepare_session` every
time a prompt is assembled, so the inspectable copy can never diverge from what
the PM received (#382).

## Related Skills

- `tm-ticketing` — whether an issue exists, and everything about its content and lifecycle
- `tm-git-file-tracking` — files must be tracked on the feature branch before PR creation
- `tm-delegation-patterns` — the agent-selection matrices this workflow routes into
- `tm-circuit-breaker` — CB#5 (delegation chain), CB#6 (forbidden `gh` usage), CB#8 (QA/review gate)
- `tm-verification-protocols` — the QA evidence standard every phase gate uses
- `tm-agent-architecture` — how the agents this workflow delegates to are built
