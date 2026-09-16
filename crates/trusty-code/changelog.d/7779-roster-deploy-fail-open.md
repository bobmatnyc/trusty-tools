Fixed

- A roster deploy that could not read one of its own staged files no longer reports success ([#7779](https://github.com/bobmatnyc/trusty-tools/issues/7779))
  - the publish step skipped an unreadable scratch file while still writing the ledger that recorded its checksum, so `tcode` logged an agent as deployed that was not on disk; it now reads every file before writing any and fails with `RosterDeployError::Publish`
  - a deploy refused for a corrupt ledger no longer leaves an empty `.trusty-code/skill-refs/` behind, which path resolution would have read as a usable native configuration root
