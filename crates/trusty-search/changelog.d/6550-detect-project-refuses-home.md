Fixed

- `detect_project` refuses to derive an index id from the home directory or the
  filesystem root instead of using their basename (#6550). Running a
  project-scoped command from `$HOME` derived an index named after the
  operator; it now returns `ProjectRootRefused`, which the CLI reports with the
  refused path and a pointer to `--index <id>`. `detect_project` and
  `index_resolve::resolve_index` are fallible as a result.
