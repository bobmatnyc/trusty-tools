Fixed

- A plain directory on its own filesystem (for example a tmpfs `/tmp`) is now
  classified as having no git repository. git reports
  `not a git repository (or any parent up to mount point <dir>)` when its
  discovery stops at a mount boundary, and the work-tree probe only recognised
  the `(or any of the parent directories)` wording, so the reconcile mtime
  path treated such roots as `Unknown` (#9475).
