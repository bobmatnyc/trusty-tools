# Plan: factory intake by issue status signal

Status: plan only, no code. 2026-11-18. Start 2026-11-18T16:30Z, end 2026-11-18T16:31Z. Estimate 0.5 h. Owner instruction 2026-11-18T16:21Z. Build follows #11470.

## Findings (path:line)
1. The "factory" is a set of sessions, not an issue picker. `~/trusty-mpm-projects/architect/scripts/factory_tasks.py:1-30` seeds one recurring task per factory session; its standing actions (`:51-56`) say "Review every open issue ... adversarially review issues it did not file". No script chooses which issue to take; the session decides. `tm-issues-prune` (`content/skills/tm-issues-prune.md:155-183`) ranks by `P0`-`P3` label and suggests next, and no `tm issue next` exists.
2. Board Status is read in three places, none in an intake path. `crates/trusty-mpm/src/bin/tm/commands/issue/board/snapshot.rs:125-157` (board shape), `:236-270` (all items, one page of 100 per call, `fieldValueByName`), `:322-340` (open issues with labels). Architect `scripts/board_feed.py:9,51-54` counts items by Status, about 1 point per page of 100. `status:*` labels are read by `issue/state.rs:225-262` (`resolve_current_state`: None / One / Many).
3. Lifecycle (`TICKETING.md:154-171`): Backlog, Ready for Development, In Development, In PR, On Preview, Merged, On Prod, Published, Won't Do. Ready for Development has NO label ("none", `:161`). No `status:ready` label exists in the repo; the signal today is board Status only.
4. Goal order is the Intent Ledger `rank` of active goals (`datg.py:1206`, `:1270`), ids `ar-G*`. Nothing links an issue to a goal in the issue data.
5. Cost: `read_items` is one GraphQL point per 100 items, so a 1,000-item board is about 10 points and `gh issue view` per issue is 1 point each. The shim refused a call today on the primary GraphQL limit, reset 17:27:50Z (`crates/trusty-mpm/src/core/gh_session_shim.rs:286-287`).

## Intake rule
Take an issue only when its board Status is Ready for Development, or it carries `status:ready` (to be created, see Q1), while both are maintained. Read Status in one batched query per board (`snapshot.rs:264` shape plus `content{... on Issue{labels(first:20){nodes{name}} state}}`), 100 items a page, cached for 10 min under the architect inbox with the board `updatedAt`; never one call per issue. Skip closed issues.
- Tie-break: goal order (lowest `rank` of the goal the issue serves), then `P0`<`P1`<`P2`<`P3`, then oldest `createdAt`. An issue with no goal sorts after every goaled one.
- No signal (Backlog, no Status, no label): not taken. The session may raise it to the Architect to groom.
- Conflict (Status and label disagree, or two Status values): not taken; report once as a `tm issue repair` candidate. Board Status wins only after the labels are retired (`TICKETING.md:212`).
- Missing acceptance criteria: not taken. The audit warns on it (the plan `docs/plans/issue-acceptance-criteria.md` row 6, #11469); intake reads the same audit result, so #11469 lands first or in the same PR.
- Adversarial review (`factory_tasks.py:53`) and AC rules still run on a taken issue.

## Files to change
| # | File | Change | Rung |
|---|---|---|---|
| 1 | `crates/trusty-mpm/src/bin/tm/commands/issue/board/snapshot.rs` | batched Ready-list read with labels; 10 min cache | 4 |
| 2 | `crates/trusty-mpm/src/bin/tm/commands/issue/` new `next.rs` + `mod.rs` | `tm issue next --board N` applies rule and tie-break, prints ranked list | 6 (CLI surface) |
| 3 | `crates/trusty-mpm/src/core/issue_audit.rs` | expose the AC finding to `next` | 5 |
| 4 | `architect/scripts/factory_tasks.py:51-56` and `content/skills/tm-issues-prune.md` | COMMON_ACTIONS and Suggest Next call `tm issue next` | 3 |
| 5 | `TICKETING.md:154-171`; `content/agents/ticketing.md` | state the intake rule and the new label | 1 |
| 6 | tests, `changelog.d` fragment | regression: unlabeled and conflicting issue not taken | 3 |

## Estimate
Engineer 6 h (rows 1-3, 6), agent 4 h (rows 4-5, golden refresh, rate-limit-safe live check). Planning 0.5 h.

## Open questions for the owner
1. Create a `status:ready` label, or read board Status only (the table says "none")?
2. How does an issue map to a goal: a `goal:ar-G*` label, the board's milestone, or the repo's goal in the ledger?
3. On conflict, should the board Status win now, instead of skipping?
4. Does "take" cover an issue assigned to a live session already (skip), or only unassigned?
5. Cache path and TTL acceptable, given the limit reset at 17:27Z?
