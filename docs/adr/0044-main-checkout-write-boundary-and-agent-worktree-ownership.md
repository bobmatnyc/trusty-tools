# 0044. Main-checkout sessions restrict writes; the harness owns agent worktrees

- **Status:** Amended by [0048](0048-dispatched-writers-get-a-worktree-and-the-write-boundary-is-enforced.md), [0056](0056-main-checkout-write-access-is-granted-by-role.md)
- **Date:** 2026-08-10
- **Scope:** crate `trusty-mpm` session launch and delegated-agent enforcement;
  Claude Code worktree isolation under `.claude/worktrees/`
- **Reversibility Cost:** Medium — the write boundary is user-visible and must
  be enforced across both PM and delegated-agent execution paths
- **Decision Drivers:** owner ruling that main checkouts are read-only except
  for documents and configuration; verified absence of a trusty-mpm agent
  worktree creation path; ADR-0036's harness-owned worktree topology
- **Current amendment:** [ADR-0048](0048-dispatched-writers-get-a-worktree-and-the-write-boundary-is-enforced.md) builds the mechanism decision 2
  requires, grants a dispatched writer the harness worktree decision 4 keeps
  trusty-mpm out of creating, and extends decision 5's row 4 so isolation may
  be requested on a dispatch's behalf. Decisions 1-4 and 6 stand as accepted.
- **Supersedes / Superseded by:** Amends ADR-0037's write boundary and corrects
  row 4 of its placement table. ADR-0037's PM placement rules remain in force.

## Context

ADR-0037 decided where a PM session runs. Two findings recorded after its
acceptance need their own immutable decision record rather than an in-place
normative amendment.

First, the owner ruled that a session on the main checkout is read-only except
for documents and configuration. The restriction applies to the PM and every
agent it dispatches. Framework deployment (`.claude/`, bundled skills, and
`TASK.md`) is configuration and remains permitted. Source changes are not.

Second, ADR-0037's fourth placement-table row said a dispatched agent receives
its own worktree through trusty-mpm when the project `worktree` flag is true.
Code inspection disproved that statement. No trusty-mpm production path creates
an agent worktree. Claude Code creates agent worktrees through
`Agent(isolation: "worktree")` or `EnterWorktree`, under `.claude/worktrees/`
as established by ADR-0036. The one production use of
`worktree_enabled_for_origin_at` gates fallback framework deployment into the
checkout where the operator is standing; it is a permission check, not
worktree creation.

## Decision

1. A PM session running on a project's main checkout, and every agent it
   dispatches, may write documents and configuration only. Source changes are
   forbidden.
2. The restriction is enforced mechanically across PM and delegated-agent
   execution paths. Convention alone is insufficient.
3. Framework deployment remains permitted configuration: `.claude/` refreshes,
   bundled skill deployment, and `TASK.md` may be written on launch.
4. Trusty-mpm does not claim to create worktrees for dispatched agents. Agent
   worktree isolation belongs to the harness and uses `.claude/worktrees/` per
   ADR-0036.
5. ADR-0037's placement-table row 4 is replaced by: "Dispatched agent | any
   project flag | explicit harness isolation only | harness-owned worktree when
   requested; otherwise the session's checkout." Rows 1–3 remain unchanged.
6. The per-project `worktree` flag has no role in PM placement or agent
   worktree creation. Its live effect is limited to the daemon-unreachable
   fallback permission for framework deployment.
7. **A project may declare that it holds no source (#7905).** Decision 1's
   "documents and configuration" is decided by file EXTENSION, which is a proxy
   for "a change another session standing in this tree could be building on". A
   prose repository has no such change, and the ones whose own CLAUDE.md forbids
   worktrees have no second place to write either — so a `.py` helper beside an
   article in `bobmatnyc/writing` was unwritable AND uncommittable at once, and
   a `git mv` of it into the archive could be landed by neither route. A project
   states the exception once, as `documents_only = true` in the committed
   `.trusty-mpm.toml` (ADR-0042's project-level surface), and the two rules that
   consult it — this ADR's write boundary and ADR-0049's commit gate — read it
   through ONE function so they cannot disagree about whether a file may be
   written but not committed.

   **The declaration counts only when it is COMMITTED AT `HEAD`, and landing it
   is itself a source-class commit.** Both halves are required; each closes the
   other's gap, and the first cut of this decision shipped neither. A
   `.trusty-mpm.toml` is not source under `is_source_code_path`, so writing it is
   admitted by the very boundary it switches off — on the built binary, `Write
   .trusty-mpm.toml` followed by `Write src/lib.rs` defeated this ADR in two tool
   calls. So the grant is read from the blob at `HEAD:.trusty-mpm.toml`, never
   from the working tree: an untracked declaration, a staged-but-uncommitted one,
   and an uncommitted edit to a tracked one all grant nothing. That alone would
   not have been enough either, because `.trusty-mpm.toml` is configuration and
   ADR-0049's commit gate read `git add .trusty-mpm.toml && git commit` as an
   ordinary documents commit — one command, from the main checkout, and the
   declaration was at `HEAD`. So a staged change that INTRODUCES, FLIPS or
   RETRACTS `documents_only` is classified as source at that gate and refused
   there. A staged edit to that file that leaves the key alone stays an ordinary
   documents commit.

   **Committing is not the only way `HEAD` moves, so every verb that moves it is
   gated too.** The first cut of this decision claimed the two rules above left
   exactly one route to declare a project, and that claim was false on the built
   binary. The declaring commit does not have to be MADE in the main checkout —
   it only has to ARRIVE there. `git merge other/declare-branch` was allowed, no
   commit gate saw it, and `git show HEAD:.trusty-mpm.toml` read `documents_only
   = true` immediately afterwards. The ADR-0048 decision 10 head-move rule did
   not catch it either: that rule asks the daemon whether another session is
   standing in the tree, so a solo session or an unreachable daemon always
   allowed, and it covers `merge` and `rebase` only. So a third rule, with no
   daemon round-trip and no live-writer condition, denies any of `merge`,
   `rebase`, `cherry-pick`, `revert`, `reset` (any mode), `checkout -B`/`-b`/
   `--orphan`, `switch -C`, `update-ref` on `HEAD` or `refs/heads/*`,
   `symbolic-ref HEAD`, `am`, and `apply --index` in a main checkout when the
   revision it would land carries a different `documents_only` value than `HEAD`
   does. It proves UNCHANGED rather than detecting CHANGED: a named revision
   that does not resolve denies, and `am`/`apply --index` name no revision at
   all and therefore always deny there. Verbs that cannot move `HEAD` — `fetch`,
   `log`, `show`, `status`, `diff` — are untouched, so the declaring commit may
   still be fetched and read.

   With all three in force the invariant holds as stated: a project's
   `documents_only` value can only change in a main checkout by way of a
   worktree branch and a reviewed pull request.

   The declaration empties the SOURCE CLASS for that checkout and relaxes
   nothing else. ADR-0049 decision 3's live-writer check, decision 5's
   empty/unreadable-index denies, decision 8's lone-command rule, ADR-0048's
   destructive-git rules and every secret-file rule are untouched. It is read
   from the checkout ROOT the guard already resolved, never from the working
   directory, and it fails closed: an absent file, a `false` value, an
   unreadable file and one that fails `deny_unknown_fields` all leave the deny
   exactly as it was, so a declaration that cannot be trusted can never widen
   anything. It is a SEPARATE key from `agent_worktree` and does not imply it —
   that one says where dispatched agents stand, this one says what the
   repository contains.

## Consequences

- Main-checkout sessions can safely support writing projects and configuration
  maintenance without granting source-write authority.
- Enforcement must cover delegated agents; a PM-only guard does not satisfy the
  decision.
- Worktree ownership and placement now agree with ADR-0036 and the actual
  harness boundary.
- Existing ADR-0037 prose and its original table remain historical context;
  this ADR is controlling for the write boundary and row 4.

## Related Decisions

Vetted against the ADR corpus on 2026-08-11:

- **ADR-0036 (All worktrees under `.claude/worktrees/`):** Extends — assigns
  agent worktree creation to the harness at the topology ADR's chosen location.
- **ADR-0037 (PM placement precedence):** Amends — adds the write boundary and
  corrects the agent row without changing PM placement.
- **ADR-0030 (Session/workstream model, Proposed):** Consistent — does not alter
  its proposed session-to-workstream relationship.

No prior Accepted decision contradicts this amendment.
