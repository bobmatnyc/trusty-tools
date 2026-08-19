Fixed
- A pinned install no longer places a partial set and calls it complete. When a
  binary the set named was absent from the staged extraction directory,
  `copy_set_into_install_dir` skipped it silently and still returned `Ok`, so
  the caller received a set missing a tool with no warning anywhere. The copy
  phase now fails closed on that name, before the first commit rename, so its
  "nothing was installed" text stays true (#5810).
