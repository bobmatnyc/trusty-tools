Changed

- The dependency inventory now resolves a declared range against the checkout's
  lockfile and records which file answered. `poetry.lock`, `uv.lock` and the
  `==` pins of `requirements.txt` are read for the first time, so a
  `pyproject.toml` project no longer reaches the report as ranges only —
  previously every python row was unresolved, the largest share of the 515 of
  1230 unscannable rows a 59-repository run produced. `Dependency` gains
  `resolved` (true only when the locked cell is one exact version) and `source`
  (the filename it came from); the Dependency Inventory table gains a
  `Resolved from` column that reads `not resolved` when no lockfile answered.
  A lockfile that is present and fails to parse no longer degrades silently: it
  is named in `DependencyInventory::lockfile_warnings` and rendered under the
  section, and the pass carries on with that ecosystem's declared ranges. Only
  the resolved name/version pairs are kept, so the inventory does not grow by
  the size of the lockfile it read. `Dependency` and `DependencyInventory` are
  now `#[non_exhaustive]` (issue #6794).
