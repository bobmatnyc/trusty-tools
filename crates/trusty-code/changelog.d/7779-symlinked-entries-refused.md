Changed

- A symlinked file or directory anywhere beneath `<project>/.trusty-code/` now refuses the roster deploy instead of being followed ([#7779](https://github.com/bobmatnyc/trusty-tools/issues/7779))
  - this narrows "a hand-edited deployed file is authoritative" to real files: a symlinked `agents/<role>.md`, skill folder or ledger reports `WriteTargetError::Unpinned` and nothing is written, where earlier releases read through the link and preserved it
  - publishing replaces a link with `renameat` rather than writing where it points, so following it on read and replacing it on write would have disagreed about what the deploy did; keep the real file under `.trusty-code/` and link to it from elsewhere
- The project ledger lock is a blocking `LOCK_EX` held across the whole mirror/stage/compose/publish sequence, wider than the shared deployer's compose-only lock, so the snapshot that decides "hand-edited" and the publish that acts on it see the same directory
