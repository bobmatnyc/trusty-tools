Added

- `UsearchStore::try_index_size()` returns the live vector count as a `Result`, for a caller that needs to tell a read failure apart from a genuinely empty index. `index_size()` keeps its existing `unwrap_or(0)` degrade for its purely-informational callers and now delegates to this new method ([#6376](https://github.com/bobmatnyc/trusty-tools/issues/6376))
