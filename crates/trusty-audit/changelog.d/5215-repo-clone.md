Added

- `trusty-audit clone <owner/name>...` acquires repositories the recipient has
  never checked out, into the working directory's `repos/` area — via
  `gh repo clone`, reusing the credential `gh auth login` already resolved. A
  clone is built in a `.partial` sibling and renamed into place only after `gh`
  exits zero, so an interrupted run never leaves a half-checkout a later stage
  would analyze as whole. One repository failing is a named gap and the run
  continues; only every repository failing aborts it. Disk use is shallow by
  default, bounded at 20 GiB, and reported (#5215, DOC-68 §8 / §14 Q2).
