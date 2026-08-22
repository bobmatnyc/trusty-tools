Added

- `derive_checkout_index_id` derives a collision-resistant trusty-search index
  id for one checkout: the slugified basename plus 8 hex digits over the
  canonical path, e.g. `trusty-tools-3fa9c1d2`. `derive_index_id` is the bare
  basename, so two checkouts of one repository — an audit engagement's clone and
  the operator's own tree — collide on a single id, and trusty-search's registry
  is one `root_path` per id: the second checkout is silently served the first
  one's content. It is deliberately pure, a function of the path and nothing
  else, because `trusty-audit`, `trusty-review` and `tga` each derive this id in
  a separate process and a component read from live git state (as
  `derive_project_index_id` reads origin and operator) would let two of them
  disagree. A path that cannot be canonicalised is normalised through
  `Path::components`, so a trailing separator and a `path = "."` entry's
  `<base>/.` still name one tree. `None` for a path with no final component.
