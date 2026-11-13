Fixed
- The live lane (`tests/live_lane.rs`) checks for the `[[gdocs_grant]]` row for store `lane-docs` before any import and fails at once, naming the row and the grant file; before, it ran the mail imports and failed late as `policy_refused` in the Docs outcomes (#10746). Test-only; no behaviour change.
