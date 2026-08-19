Changed

- `report` now renders two documents per run instead of one (#6046): the
  code-review report keeps every section it had except Authorship & Key-Person
  Risk, which becomes its own `<stem>-authorship.md` beside it, with its own
  title, provenance legend, and generation date. Both come from one fill pass,
  so the two cannot drift in wording or provenance tagging. The code-review
  report's Contents jump list no longer links the authorship section, since it
  no longer carries it. A run with no authorship data still writes the second
  document, carrying the same no-data line the section used to show inline.
  Both paths are printed to stdout as before, so `tga audit` picks them up and
  `trusty-audit` packages both without change.
