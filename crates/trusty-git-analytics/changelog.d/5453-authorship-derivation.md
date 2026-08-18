Added

- `tga audit` writes one authorship artifact per repository (#5453, #6004):
  ownership concentration, bus factor, single-author subsystems, and a
  trailing-12-month monthly trajectory, derived from `commits JOIN files`.
  Bot commits and merge commits are excluded from every figure; squash-merge
  attribution, `.mailmap`-free identity merging, and vendored-path exclusion
  are NOT corrected for and are caveated verbatim in the artifact (derivation
  lives in tga per #5468's ruling, never in trusty-review). The DD manifest's
  `[[repositories]]` entries gain a new, additive `authorship` path field
  mirroring the DOC-67 §8 `velocity` field precedent; a repository whose
  artifact fails to write states that as a named gap on the manifest rather
  than aborting the run.
