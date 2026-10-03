Changed

- `trusty-review run` on a GitHub PR checks a pinned index against the PR's
  repository instead of using it unchecked. A `TRUSTY_SEARCH_INDEX` recorded
  for the PR's repo is used; one recorded for another repo, not registered, or
  with no recorded identity (unless its id is the repo name) fails the run with
  an error naming both repos and how to clear the pin. The index
  `--source-root` maps to fails the run when it is recorded for another repo;
  a legacy index with no recorded identity is used with a warning. Local
  `--local-diff`/`--base` runs are unchanged.
