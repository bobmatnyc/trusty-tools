Fixed

- CLI GitHub auth skips a trusty-mpm `tm-` placeholder in `GITHUB_TOKEN` or `GH_TOKEN` and falls through to `gh auth token`, instead of sending it to GitHub and failing with 401.
