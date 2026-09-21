Added

- New test-only workspace member (`publish = false`) that hosts the tests spanning two production crates, so neither has to dev-depend on the other ([#8341](https://github.com/bobmatnyc/trusty-tools/issues/8341))
  - the AC-18 cross-gate golden test moves here from trusty-mpm
  - the trusty-analyze UDS consumer contract moves here from trusty-analyze
  - tctl's live-daemon probe arm moves here from trusty-memory
