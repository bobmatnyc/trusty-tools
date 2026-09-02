Changed
- `tm issue states` prints `(no label)` for a label-less state and marks the
  edges that require a `--note`.
- The bundled `tm-ticketing` skill replaces every hand-typed
  `gh issue edit --add-label … --remove-label …` lifecycle example with the
  `tm issue transition` form, keeping the single-`gh issue edit` fallback for a
  host without `tm`.
