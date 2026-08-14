Fixed

- `tm-ticketing` skill: removed the `Provenance` label family, which listed
  `trusty-mpm` as a harness default and let the ticketing agent apply that label
  to any issue it filed regardless of which crate the defect lived in. The skill
  now states three label families and rules out an origin-based axis under any
  name — provenance, umbrella, or dogfooding — matching the agent asset fixed in
  #5691.
