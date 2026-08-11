Added

- Agentic detection reads operator-supplied markers from a YAML file, so a target
  org's house footer is detectable without a code change or a tga release (#5414).
  The path comes from `TGA_AI_MARKERS`, defaulting to `~/.config/tga/ai-markers.yaml`;
  each entry is a `tool` label, a `mode` (`full_agentic` / `ide_assisted`), a `scope`
  (`trailer` / `message` / `email`), and a `regex` pattern.
  - New public module `collect::ai_marker_config` (`MarkerConfig`, `MarkerSpec`,
    `MarkerMode`, `MarkerScope`, `MarkerConfigError`, `marker_file_path`). Deliberately
    a separate type rather than a field on `Config`, which would have forced a major bump.
  - The shipped markers are scanned to completion first, and operator markers are
    consulted only for a commit they left unmarked. An operator marker can therefore
    classify a commit the shipped set missed, and can do nothing else to one it already
    catches — including a `full_agentic` operator entry against an `ide_assisted`
    builtin verdict, which appending alone did not prevent.
  - A marker file that cannot be read, parsed, or compiled — or that declares more than
    256 markers — is rejected whole, logged at `warn!`, and named in
    `detection_disclosure()`; the run continues on the builtin markers.
  - `detection_disclosure()` now states how many operator markers were active and where
    they came from, so an `agentic_pct` figure can be read against the run that produced it.
