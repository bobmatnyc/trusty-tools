Fixed

- The orphan-index sweep no longer reads a failed status probe as an empty
  index. It substituted `chunk_count: 0` for a refusal or an unanswered call, and
  `0` is exactly the reading that makes an aged, unclaimed `.worktrees` index
  collectable — so a wedged or restarting daemon could license a delete on
  evidence that was never gathered. A candidate whose chunk count cannot be read
  is now skipped, and a listing that goes unanswered ends the sweep with nothing
  collected rather than erroring.
- A destructive index delete is reported as removed only when the daemon says the
  registration is gone. Any success answer used to count, including the #3049
  body that reports `removed: false` because an in-flight writer never quiesced —
  so the sweep recorded an index as reclaimed while it was still registered and
  still on disk.
