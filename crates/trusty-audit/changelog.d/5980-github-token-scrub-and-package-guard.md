Fixed

- The scrubber that keeps a credential out of a spawned child's log, and the
  guard that refuses to package a file carrying one, both now cover the
  `gh`-derived GitHub token — previously only `EngagementConfig`'s own
  secrets were checked, so a rejected token echoed back by a child could
  reach the log or the deliverable unredacted (#5980).
