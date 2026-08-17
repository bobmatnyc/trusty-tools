Added
`tm doctor --fix` now redeploys drifted or missing output styles under `~/.claude/output-styles/`, honouring the existing dry-run-by-default / `--yes` convention. A file that cannot be read is refused rather than overwritten. (#5866)

Fixed
The `output_style_staleness` and `output_style` checks no longer tell the operator to run `tm install`, which has no output-style step — the deployed file kept its mtime across a full install run. Both now name `tm doctor --fix --yes`, and the drift scan behind the report is the same one the repair acts on. (#5866)
