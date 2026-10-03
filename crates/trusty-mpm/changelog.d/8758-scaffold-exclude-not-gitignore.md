Fixed

- `tm session launch` no longer edits a project's tracked `.gitignore`, so
  `git status` stays clean after a launch. The harness-scaffolding paths
  (`.claude/agents/`, generated output styles, settings lock files,
  `.trusty-mpm/sessions/` and the rest) now go to the repository's shared
  `.git/info/exclude`. A managed block already in a `.gitignore` is left as
  it is. The launch no longer reads `.gitignore`, so a FIFO there cannot
  block this step. The `scaffold_tracking` doctor remediation now says the
  paths stay excluded through `.git/info/exclude`.
  `core::scaffold_gitignore::ensure_scaffold_gitignored` and its block
  refresh are removed, so no code path rewrites a tracked `.gitignore`: the
  refresh used to drop operator lines inside the managed block and convert
  CRLF line endings to LF.
