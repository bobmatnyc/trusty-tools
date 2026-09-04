Fixed

- `investigation.json` now carries every dependency a repository declares. The
  inventory was truncated to 30 rows before it was serialised, so a machine
  consumer reading `repos[].deps` — trusty-audit's OSV vulnerability lookup —
  saw 30 packages for a workspace declaring 134 and reported partial coverage
  as complete. The 30-row cap now applies only when the markdown Dependency
  Inventory table is rendered, so the report page and its "and N more" line are
  unchanged (#6788).
