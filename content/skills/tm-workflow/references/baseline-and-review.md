# tm-workflow — baseline failures and the review gate

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

   Never report "all tests pass" while a gate is red, whoever caused it. A
   branch-caused failure blocks the merge whatever branch protection requires
   (#8614). A pre-existing failure of a non-required check merges only when the
   brief waives it by name (`tm pr merge <PR> --allow-failing <check>`). A red
   required check is never merged around.

## Critic before PR open

`code-critic` reviews the committed branch before `gh pr create` or
`tm pr open`. Its verdict is posted on the PR as a comment by an OWNER, MEMBER
or COLLABORATOR, with the verdict line `## Verdict: <V> pinned to <headsha>`.
The body's `## Review` field cites that comment. A later push needs a new
verdict pinned to the new head. `tm pr queue-check` refuses a `rung:5` or
`rung:6` PR without one (owner ruling Dr4).

## The trusty-review Gate

Before merge, delegate to the review gate:

```
mcp__trusty-review__review_diff   # in-progress diff review
mcp__trusty-review__review_pr     # open-PR review (correctness/design/security)
```

This is CB#8's evidence for a PR-shaped completion claim — "the PR is ready"
requires a review verdict (or human approval), not just green CI. Its MEDIUM and
LOW advisories follow the Completion Standard fix bar: one comment, no fix round.
