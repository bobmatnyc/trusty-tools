Fixed

- The `tm-ticketing` skill's confidence-state marker (`Observed` /
  `Reproduced` / `Inferred` / `Speculative`) is now scoped to defect (`bug`)
  issue bodies. It used to be required on every filed issue, which produced
  lines like `Confidence: Observed … — accepted feature work` on feature
  tickets — a restatement of the type label that carries no information,
  since a feature ticket has no "reproduced vs. suspected" axis to report.
  Bug tickets are unaffected: the marker still distinguishes a reproduced
  defect from a suspected one there, which changes what a reader does next.
