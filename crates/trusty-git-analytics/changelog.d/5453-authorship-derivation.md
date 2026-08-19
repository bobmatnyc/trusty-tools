Added

- `tga audit` writes one authorship artifact per repository (#5453, #6004):
  ownership concentration, bus factor, single-author subsystems, and a
  trailing-12-month monthly trajectory, derived from `commits JOIN files`.
  Bot commits and merge commits are excluded from every figure. Authors are
  grouped by the collection pass's own identity resolver
  (`commits.author_id → authors.canonical_email`), so one person's aliases
  collapse to one author; a commit the resolver never linked keeps its raw
  email and is counted into the new `unresolved_authors` figure, which earns
  its own caveat naming the count. Squash-merge attribution and vendored-path
  exclusion are NOT corrected for and are caveated verbatim in the artifact
  (derivation lives in tga per #5468's ruling, never in trusty-review). The DD
  manifest's `[[repositories]]` entries gain a new, additive `authorship` path
  field mirroring the DOC-67 §8 `velocity` field precedent; a repository whose
  artifact fails to write states that as a named gap on the manifest rather
  than aborting the run.
- A repository name that matches no `commits.repository` value now states a
  named gap instead of rendering "0 authors, bus factor 0" (#5453). The
  aggregation cannot tell an unmatched name from an empty repository — both
  count zero rows — so `tga audit` probes for the name first and, when nothing
  matched, names the repository names the database actually holds.
