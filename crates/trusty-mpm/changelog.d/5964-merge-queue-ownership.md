Added

- The PM instruction package's workflow section now states merge-queue ownership: one
  session owns a repository's merge queue at a time, a merge authorization is scoped to
  the PRs presented when it was given, an outstanding review verdict (a `code-critic`
  BLOCK, a requested-changes review, a hold label) blocks a merge that green required
  contexts would otherwise permit, and a hold is marked in GitHub state rather than
  announced by message. Procedure in the `tm-workflow` skill; pointer from
  `tm-delegation-patterns`' Cross-Workstream Coordination.
