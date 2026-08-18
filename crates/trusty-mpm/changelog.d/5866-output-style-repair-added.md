Added

- `tm doctor --fix` now redeploys drifted or missing output styles under `~/.claude/output-styles/`, honouring the existing dry-run-by-default / `--yes` convention. A file that cannot be read is refused rather than overwritten ([#5866](https://github.com/bobmatnyc/trusty-tools/issues/5866))
