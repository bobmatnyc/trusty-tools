# tm-ticketing — relationships

## Relationships (native, never prose)

Use GitHub's own sub-issue and dependency APIs for every issue relationship.
Never express one only in prose — a task list, a "see also", or a `Closes #N`
used for anything but the literal fix link.

- Sub-issue: parent/child — an epic, a milestone tracking issue, an umbrella
  bug with individual fixes underneath.
- Blocked-by: sequencing — this issue cannot land until that one does, no
  parent/child implied.
- `Refs #N` in a PR body: fix linkage only, never an issue-to-issue relationship.
- "Companion" or "related" in prose is not a relationship. Use sub-issue,
  blocked-by, or nothing.

**Commands.** For parent/child, `gh issue create --parent <n>` and
`gh issue edit <n> --parent <p>` take the parent's ISSUE NUMBER and are the
shortest path (gh 2.96). The API forms below take the child/blocker's numeric
database id instead, and remain the only route for blocked-by. `-F` sends a
field as an integer; `-f` sends it as a string and the call fails.

```bash
# parent/child, by issue number
gh issue create --title "…" --body "…" --parent 7067
gh issue edit 7070 --parent 7067

CHILD_ID=$(gh api repos/OWNER/REPO/issues/CHILD_NUM --jq .id)   # number -> id

# sub-issue: add / remove
gh api --method POST repos/OWNER/REPO/issues/PARENT_NUM/sub_issues -F sub_issue_id="$CHILD_ID"
gh api --method DELETE repos/OWNER/REPO/issues/PARENT_NUM/sub_issue -F sub_issue_id="$CHILD_ID"

# blocked-by: add / remove (remove takes the id in the path, not a body field)
BLOCKER_ID=$(gh api repos/OWNER/REPO/issues/BLOCKER_NUM --jq .id)
gh api --method POST repos/OWNER/REPO/issues/ISSUE_NUM/dependencies/blocked_by -F issue_id="$BLOCKER_ID"
gh api --method DELETE repos/OWNER/REPO/issues/ISSUE_NUM/dependencies/blocked_by/$BLOCKER_ID
```

**On filing.** Every relationship the brief names is set in the same dispatch
that files the issue — alongside the milestone and the project — and reported:
"filed #N as a sub-issue of #P", or "filed #N, blocked-by #B". A relationship
left for a follow-up pass is a relationship that does not exist.

**On closing a parent.** List its open sub-issues first. Refuse to close while
any are open unless the user explicitly says to close anyway.

**Reading them back:**
```bash
gh api repos/OWNER/REPO/issues/PARENT_NUM/sub_issues --paginate --jq '.[].number'
gh api repos/OWNER/REPO/issues/ISSUE_NUM/dependencies/blocked_by --paginate --jq '.[].number'
```

`gh api` pages at 30 items by default — add `--paginate` (used above) on every
listing call.

### Epics and phases

A tracker is titled `[EPIC <epic#>] <outcome>` and each phase
`[EPIC_<epic#> PHASE_<n>] <what>`, where `<epic#>` is the tracker's own number
— file the tracker, read the number back, edit it into the title. Phases are
native sub-issues (`--parent <epic#>`), each with a type label from the
six-value set and the parent's milestone. The tracker body carries three marker
blocks — `phases` (regenerated wholesale from child state, never hand-patched),
`deferred` (scope removed from the plan) and `followups` (findings surfaced
during execution). The gate test, the four rules, the templates and the manual
`gh` sequence are in `tm-epic`.
