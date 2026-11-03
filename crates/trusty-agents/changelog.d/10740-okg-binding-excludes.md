Fixed
- The protected OKG index binding admits every concept path except `_sources/**` and `_state/**`, stated as literal `exclude_globs` with no `notes`-only allowlist; an existing binding gains both globs on its next bind and its already indexed `_sources/` and `_state/` files are removed (#10740).
