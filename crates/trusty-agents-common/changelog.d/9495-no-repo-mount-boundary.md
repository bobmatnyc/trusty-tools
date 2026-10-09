Fixed
- The VCS-claim gate now reads a plain directory on its own filesystem (for
  example a tmpfs `/tmp`) as having no repository. git reports
  `not a git repository (or any parent up to mount point <dir>)` when
  discovery stops at a mount boundary, and the classifier only recognised the
  `(or any of the parent directories)` wording, so such a tier read as
  `Unknown`. An ancestor `.git` still refuses (#9495).
