Added
- `tm doctor --fix --dry-run` states the preview explicitly and refuses `--yes`; `tm doctor --fix --only <check>` (repeatable) runs only the named checks' repairs, in their fixed order, and an unknown check id is an error. The dry-run hint keeps the `--only` scope (#10425).
- `tm doctor --fix` prints a `NOT EVALUATED` line for each repair it could not run (no cwd, no home, no managed `CLAUDE_CONFIG_DIR`, an undetermined Rust build environment) instead of omitting it, so a preview never reads as "no change" for a repair it did not evaluate (#10425).
- `tm validate --repair --dry-run` prints the gaps and the repair `--repair` would run, and writes nothing (#10425).
