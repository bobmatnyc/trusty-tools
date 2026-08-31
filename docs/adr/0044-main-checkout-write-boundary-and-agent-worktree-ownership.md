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
