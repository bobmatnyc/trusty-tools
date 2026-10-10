Added

- `policy::load_effective_until(req, deadline)` loads the effective policy with git bounded by the caller's deadline. Each git step runs for at most the lesser of 10 s and the time left, and at the deadline its whole process group is killed. Every project not yet loaded is then refused as `GitTimedOut`, so none is reported effective, and the call returns soon after the deadline with no git process left running. `load_effective` is unchanged (refs [#8454](https://github.com/bobmatnyc/trusty-tools/issues/8454))
