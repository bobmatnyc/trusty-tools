Changed
- The `tm tui` service view now shows each `by_*` breakdown in `console_metrics` as a nested table of its top five entries, largest first, with a `… N more` row, instead of `{N fields}`. Plain object fields expand in place, and a list cut at the 40-line limit ends in a `… N more lines` row (#10908).
