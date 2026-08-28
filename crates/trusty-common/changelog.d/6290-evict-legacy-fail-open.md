Fixed

- `evict_legacy` discarded `bootout()`'s error and read a failed `remove_file`
  as "nothing was there" (#6290), so a unit that would not go down was
  indistinguishable from one that was never there. It now verifies launchd
  actually let go after `bootout` instead of trusting its exit code.
