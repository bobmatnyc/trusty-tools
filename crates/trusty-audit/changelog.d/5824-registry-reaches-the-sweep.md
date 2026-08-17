Changed

- A registered repository target now reaches the sweep. `state/audit-targets.toml`
  had no reader until now — `run` took its input from the selection file only
  `clone` wrote — so registering a repository did nothing until someone cloned it
  by hand under the same name. The one-shot `audit` command clones what is
  registered.
- A registered board is reported as a stated gap rather than silently skipped.
  Passing one to `tga audit` would mean writing the board credential into the
  generated tga config on disk, which this client will not do.
