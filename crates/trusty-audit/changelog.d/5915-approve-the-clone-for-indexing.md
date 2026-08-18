Fixed

- The code-analysis leg reads the code again. `trusty-search` is default-deny,
  and nothing in the audit chain ever approved a clone, so `tga audit`'s
  `trusty-search index <checkout>` was refused on every repository of every
  run. It was silent: `tga audit` exits 0 whenever its sweep completed, so the
  run reported success and the report simply had nothing to say about the
  source. The sweep now runs `trusty-search index add <checkout>` before each
  child, and a refusal is a named per-repository failure rather than an empty
  section.
