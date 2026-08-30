Fixed

- A finding claiming a file is "not present in diff" no longer blocks a PR that
  contains that file. A map call sees one chunk, so it cannot see the chunk that
  adds the file it calls missing; the claim is now checked against the whole
  changeset and the finding dropped when the changeset refutes it (#1873).
- Caller-supplied `referenced_code` is pinned to the map-reduce per-file prompts
  by an end-to-end test, and the branch restores and logs at error level any
  caller context that failed to reach it instead of reviewing without it
  (#2654).
- The verdict embedded in `review_body` is reconciled to the authoritative
  top-level verdict, so a merge gate reading `BLOCK` can no longer disagree with
  an `APPROVE` printed inside the review. On the map-reduce path both the grade
  and verdict reconciles now run after the shallow-review cap (#1902).
- `review_health` and `/health` report the `dry_run` a review invoked through
  that surface actually executes with, plus a `dry_run_reason` naming the gate —
  previously they reported the raw config flag while every review ran dry and
  posted nothing (#4254).
- `report_analyze_e2e::analyze_populates_complexity_and_findings` no longer
  fails when the temp path happens to spell a dropped diagnostic's code; the
  drop check reads the structured finding rather than the whole rendered page
  (#4387).
