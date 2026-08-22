Added

- The investigation prompt names the function to look at. An `inspect_priority`
  entry may now declare `hotspot = { function, start_line, end_line, cyclomatic
  }` — trusty-audit writes it from the complexity measurement (#6145) — and the
  batch digest states it directly above that file's fenced content:
  `Hotspot: lines 40-190, fn settle_invoice, cyclomatic 31 — prioritize DD
  analysis of this function.` The line comes before the content, so the model
  reads the instruction rather than reaching it after skimming 900 lines. The
  Investigation Coverage section names the function too, beside the reason the
  file was read.
- A file with no declared measurement produces a byte-identical prompt, and a
  measurement without a usable line range produces no line at all rather than
  one pointing at lines 0-0. A half-written `hotspot` table is inert, the same
  way a priority naming no tracked file is inert — it never fails the manifest
  load. Nothing here changes which files are selected.
