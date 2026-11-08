Fixed
- `tm doctor` prints a `project: <path>` line under its header that names the project it checked, marked `(cwd)` or `(--dir)`, so a row that fails because a directory has no project settings, or a `--fix` run, shows which directory it applies to (#10565).
