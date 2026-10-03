Changed

- `trusty-review run` and `compare` now let an explicit `--source-root` win
  over `TRUSTY_SEARCH_INDEX`, which can be inherited from a parent process's
  environment. On a GitHub-PR `run`, the index `--source-root` maps to is still
  checked against the PR's repository. With no `--source-root`, a
  `TRUSTY_SEARCH_INDEX` pin is checked as before.
