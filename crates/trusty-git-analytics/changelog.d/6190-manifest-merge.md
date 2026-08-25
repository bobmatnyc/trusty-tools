Fixed

- `tga audit --output <dir>` folds its manifest into a `manifest.toml` that is
  already there instead of replacing the file. trusty-audit's grounding pass
  writes `inspect_priority`, `crate_topology`, and the `investigate_*` budget
  into that same file, and a second `tga audit` into a live engagement used to
  discard all of it silently — one run collapsed from 226 findings to 31. tga
  now rewrites only the keys it produces; an existing manifest it cannot parse
  is refused rather than overwritten.
