Fixed
- The ADR-0037 main-checkout guard no longer refuses `git checkout <sha> -- <paths>` and the other whole-tree git verbs inside a disposable clone under the session scratchpad (#8339). It applies the same canonicalized proof #7778 gave the write boundary, so a symlink from the scratchpad into a real checkout, and a `-C $VAR` the guard cannot expand, are still refused.
