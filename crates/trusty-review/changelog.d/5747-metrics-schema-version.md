Fixed

- A declared metrics JSON whose `schema_version` names a major this build does
  not read now fails the run by name instead of loading. Every field of the
  metrics schema defaults, so a renamed key rendered as a stated zero — a
  confident wrong LoC total, file count, or complexity distribution in a
  client-facing due-diligence report — while only syntactically broken JSON
  errored. A newer minor of a known major still loads, and a tag that resolves
  to no major at all is refused rather than assumed to be v0.
- A metrics JSON carrying no `schema_version` still loads, read as v0. Nothing
  in this workspace writes that artifact — `tga audit` omits the manifest's
  `metrics` key so the live `--analyze` fetch is not blocked — so the file is
  hand-authored against a field this crate documented as informational, and v0
  is the only schema it has ever had. This is where the metrics loader diverges
  from the ticketing loader, which refuses an untagged artifact because every
  producer of that one writes the tag.
