Changed
- The Architect seed table is split in two: the `scripts/` rows move to `seed_table_scripts.rs`, one `script!` row per line, so the prototype re-sync can add its script rows under the 500-SLOC cap. `tm fleet init` seeds the same files with the same bytes (#10571).
- `scripts/architect_bundle_meta.py history` reads every seed table file and takes `--from-repo <path> [--paths <dest>...]`, which records every version of each seeded script the given Architect git repo has held, so `tm fleet init` refreshes those copies instead of keeping them as edits (#10571).
