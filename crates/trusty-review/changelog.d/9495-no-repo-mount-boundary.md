Fixed
- A plain directory on its own filesystem (for example a tmpfs `/tmp`) now
  falls back to the directory walk instead of contributing an empty corpus.
  git reports `not a git repository (or any parent up to mount point <dir>)`
  when discovery stops at a mount boundary, and the `git ls-files` failure
  classifier only recognised the `(or any of the parent directories)`
  wording. An ancestor `.git` still refuses the walk (#9495).
