Added

- `reports/index.md` (and the sweep's own `out/index.md`) now carries a `##
  Gaps` section listing, per repository, every gap its `package.toml`/manifest
  recorded — a skipped secrets scan, a JIRA sync that never ran, a lost
  search-evidence tier — rendered verbatim and silent when a run recorded none.
  Previously that data existed only inside a 59-entry TOML array with no
  heading, table, or grep match anywhere in the file the package's own README
  calls "start here".
