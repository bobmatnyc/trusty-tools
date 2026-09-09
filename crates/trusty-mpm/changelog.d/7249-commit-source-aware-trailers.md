Fixed
- The bundled `prepare-commit-msg` hook now reads git's commit-source argument
  (#7249). A `merge` or `squash` commit is no longer stamped with the committing
  session's token trailers, and a `commit` source — cherry-pick, revert,
  `--amend`, `-c`/`-C` — has the original commit's stale figures stripped and
  replaced with the current session's, or stripped and left empty when there is
  no current session.
