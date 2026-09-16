Security

- Project writes under `.trusty-code/` now go through a pinned directory handle, closing the check-then-write race a swapped symlink could win ([#7779](https://github.com/bobmatnyc/trusty-tools/issues/7779))
  - `check_native_write_target` decided membership and every caller then wrote through the same path string, so a `skill-refs -> victim` swap between the two was followed — reproduced in 6 of 40 real deploys
  - the agents directory, the skill-refs tree and the legacy `.claude/` import each open their target once with `O_NOFOLLOW` and write `openat`-relative to that descriptor; a component swapped afterwards fails with the new `WriteTargetError::Unpinned` and writes nothing at the symlink's destination
