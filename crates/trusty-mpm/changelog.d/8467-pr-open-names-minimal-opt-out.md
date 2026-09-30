Fixed
- When `tm pr open` refuses a body for a missing or empty contract heading, it now also names `--minimal` as the full opt-out. The flag drops all nine headings, including `## Gates not run` and `## Partial-red accounting`. The attribution footer, the closing-keyword ban and the changelog gate still apply under it (#8467).
