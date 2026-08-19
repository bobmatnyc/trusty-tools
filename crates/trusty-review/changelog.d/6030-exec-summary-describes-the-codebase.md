Changed

- The synthesized executive summary now describes what the audited codebase is
  and does, and analyzes its major components, before covering risk (#6030) —
  it was risk-only. The instruction is static in the synthesis system prompt,
  like #5886's coverage-gaps note, so a template that overrides the
  `executive_summary` section instruction cannot drop it; the forced-output
  schema states the same requirement. The synthesis digest now also carries
  each application's repository-scan profile — LoC, languages, file count, and
  the detected build manifests with their declared project names and top
  dependencies — so the component analysis is grounded in real data rather
  than invented. Before this, a run without `--analyze` sent the model "No
  metrics available for this application" while the scan held all of it.
