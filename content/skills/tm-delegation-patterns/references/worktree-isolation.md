# tm-delegation-patterns — worktree isolation and concurrent dispatch

## Worktree Isolation on a Dispatch

**From a main checkout, this is automatic, not a PM choice (ADR-0048).** `tm
hook --pm-guard` grants `isolation: "worktree"` to any dispatched agent that
may write, single or parallel, the moment the session is standing in a main
checkout — the write boundary above denies that agent a source edit there
anyway.

🔴 **Declare `isolation: "worktree"` explicitly on every writer dispatch from a
main checkout anyway — the automatic grant is not reliable enough to skip the
declaration.** The guard denied a read-only-looking `qa` dispatch from a main
checkout with no explicit isolation, because `qa` authors tests and so counts
as a writer the guard didn't grant for. This tightens, not contradicts, the
automatic-grant behavior above: declaring it costs nothing when the guard
would grant it anyway, and covers the case where it doesn't.

🔴 **Do not declare `isolation` on a `version-control` dispatch (ADR-0056) —
except when the dispatch itself commits source.** For merge, push/rebase, PR
lifecycle, and worktree reclamation, `version-control` needs the checkout it
is given: a worktree removes the tree that work needs, and the isolation
fences have hidden a branch ref badly enough to force a push by SHA. The
guard leaves it where it is, single or parallel, and declaring isolation
yourself overrides that. Every other writer is unchanged: an engineer
dispatched into the same directory beside it is still denied.

🔴 **A `version-control` dispatch asked to commit a source change declares
`isolation: "worktree"` (#8156).** ADR-0044, amended by ADR-0049, forbids a
source commit in a main checkout outright; `tm hook --pm-guard` denies it
regardless of who dispatched the agent or why. Dispatching `version-control`
without `isolation` to commit source that is sitting as uncommitted edits in
the main checkout (e.g. handed back from an engineer dispatch that itself ran
without `isolation`) reaches that denial every time — the source-commit rule
above has no carve-out for it. Prefer routing the diff through the engineer's
own worktree branch so no diff goes back to the main checkout at all; when
`version-control` must make the commit itself, dispatch it with
`isolation: "worktree"` for that commit.

**From inside a worktree, pass it yourself for concurrency.** Pass
`isolation: "worktree"` on Agent tool calls when spawning 2+ parallel agents
that modify files on the SAME checkout — the case the guard cannot see,
because there is no main-checkout grant to fall back on. Not needed for
sequential agents, read-only research, or separate file trees. Use
`run_in_background: true` for fire-and-forget parallel work.

🔴 **`isolation: "worktree"` is the only sanctioned mechanism, and the PM never
tells a dispatched agent to hand-roll one — not a literal `git worktree add`,
and not prose like "work in a worktree of your own" (#5649).**
`tm hook --pm-guard` reads the declared parameter and never the prompt, so a
hand-rolled worktree leaves the agent counted against the shared HEAD and gets
the next file-mutating dispatch denied for a collision that does not exist. The
prose form recurred on 2026-09-09 in a different project after the literal-command
form was fixed here — the guard cannot see either phrasing, so both are banned,
regardless of wording.

**When `isolation` is unavailable, serialize** — one file-mutating agent at a
time, each waited for before the next. Serializing always works. Hand-rolling to
parallelize anyway is what this forbids.

**Concurrent cargo invocations in one checkout also contend, silently.** Two
agents running `cargo test`/`cargo build` in the same checkout serialize on
cargo's own `target/` build lock with no signal to either agent — one run
measured ~10 minutes for a command that normally finishes in seconds (#7895).
`isolation: "worktree"` avoids it: each worktree gets its own `target/`.

## Salvaging a Dead Agent's Worktree

A dispatch that names an existing worktree left behind by a stopped or dead
agent cannot be followed directly — `tm hook --pm-guard` refuses cross-tree
git from a differently-pinned worktree. Reach for `tm session
adopt-worktree <tree> --as <session>` FIRST, before hand-porting files or
rebuilding the branch from scratch: it is the documented verb for exactly
this case (#6497, live-verified 2026-09-02). The refusal text to search on:
`already used by worktree at .../<agent-dir>`, or a guard denial naming a
tree pinned to a different, gone session. `tm session adopt-worktree` is
distinct from `git worktree remove` (#5791, cleanup of an already-merged
tree) — adoption is for a tree still holding unmerged work.

Porting content FROM another worktree (a fix brief, a changelog fragment)
without adopting it — `git -C <other worktree>` is refused (ADR-0048). Read
its committed state with `git show <that worktree's HEAD sha>:<path>` from
your own worktree instead; see [worktree-discipline.md "Reading Another
Worktree's State"](../../../../../docs/reference/worktree-discipline.md#reading-another-worktrees-state)
for the full substitute, including uncommitted files (#7656).

Declaring `isolation: "worktree"` on a dispatch always mints a FRESH tree;
there is no parameter that says "adopt this existing one instead." A PM that
knows the target tree is a live, un-reclaimed worktree from an earlier round
still has to let the dispatched agent land in the guard refusal and run
`adopt-worktree` itself — there is no way to wire adoption in up front.

## Cross-Workstream Coordination (memory claim drawers, DOC-53)

Memory is awareness only — never a lock, never a message channel. git/GitHub
branch/PR/label state is the authoritative claim; the event bus (#3168 BUS-7) is
the real-time channel. Before dispatching multi-agent work on an area:

1. `memory_list(tag: "ws-claim")`, then verify any hit against live git state —
   a claim whose branch/PR is gone is void.
2. Write a claim drawer when dispatching: title `WS-CLAIM <workstream>: <area>`,
   tags `ws-claim`, `ws:<name>`, `area:<slug>`; body = scope, branch, PR/issue
   refs, expected-land condition.
3. Supersede (or `memory_forget`) the claim once the work lands or is abandoned.

A claim drawer covers the work area, never the merge queue. Who may merge on a
shared repository is its own claim: `Skill(skill="tm-workflow")`,
"Merge-Queue Ownership".

## Concurrent Dispatch: Declare File Ownership

🔴 Before dispatching concurrent work, name in each brief the files owned by
every other in-flight branch, and instruct the agent to STOP and report rather
than edit one. **We stack, we do not race.**

Paste a block shaped like this into every concurrent brief:

```
🔴 Files owned by other in-flight branches — do not touch
- feat/<branch-a>: core/agent_cost.rs, bin/tm/commands/pm_guard*.rs
- fix/<branch-b>: core/agent_source*.rs, core/managed_config.rs
Stop and report if you need one.
```

The doctrine is that no two in-flight PRs share a module, and that coupled
changes stack rather than race. The ownership block is the mechanism that
enforces it: an agent cannot see the other branches, so unless the brief names
their files it will edit across the line and only git finds out.

Measured on 2026-08-03: five concurrent PRs across five branches, every brief
carrying the block — zero merge conflicts, zero rebases.
