Added

- `tm doctor --dir <DIR>` scopes the project-scoped rows (`session_scope`,
  `instructions`, `agents`, `skills`, ...) to another project instead of the
  current directory. A missing or non-directory `--dir` is an error and the
  report is not printed; it never falls back to the cwd. The flag is
  report-only and conflicts with `--fix`, `--fix-skills`, `--fix-agents`,
  `--fix-launchd-secrets` and `--quarantine-mcp` (#7757).
