# doc-paths fixtures

Fixture trees for `scripts/check_doc_paths.sh`, driven by
`scripts/check_doc_paths_selftest.sh`. Each subdirectory is a whole `--root`:
the gate is pointed at it and scans it exactly as it scans the checkout, so the
scope rules, the exclusion rules and the scan floor are all exercised for real
rather than mocked.

This file sits at the fixture root, not inside any tree, so it is never itself
one of the scanned roots.

| Tree | What it pins |
|---|---|
| `broken/` | a citation naming a file that does not exist — exit 1, `FAIL BROKEN` |
| `clean/` | citations that all resolve — exit 0 |
| `excluded/` | one line per EXCLUDED TOKENS class, none of which resolves as a literal path — exit 0 |
| `crate-relative/` | a bare `src/…` token in a crate README, resolved against that crate's root: one hit, one miss |
| `line-overrun/` | a `:LINE` past the cited file's last line — exit 0 with `WARN LINE` |
| `out-of-scope/` | a broken citation in `docs/adr/`, which the scope excludes — the gate scans nothing and refuses with `SCAN FLOOR` |

`crate-relative/crates/demo-crate/Cargo.toml` is a marker, not a package: the
gate finds a crate root by walking up for a `Cargo.toml`, and the workspace's
`crates/*` glob never reaches under `scripts/`.
