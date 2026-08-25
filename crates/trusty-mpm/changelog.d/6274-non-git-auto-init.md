Added

- `tm` and `tm launch` in a directory that is not a git repository now run
  `git init` there and carry on, instead of stopping at "not in a git project"
  or "no git origin remote found"
  ([#6274](https://github.com/bobmatnyc/trusty-tools/issues/6274)). The
  repository lands in the invocation directory, a one-line
  `tm: initialized git in <path>` notice says so, and everything after that runs
  exactly as it does for a git repository with no origin remote. Three
  directories are left alone: the home directory and the filesystem root refuse
  with a stated reason, and a directory that already has a repository — its own,
  an ancestor's work tree, or a bare repo — is untouched. A missing `git`
  executable is a prerequisite error naming git; nothing is written in that case.
