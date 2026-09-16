Fixed

- The `core::memguard` RSS-liveness test no longer fails under parallel test load. It used to sample the whole test binary either side of its own 128 MB allocation, so a sibling test freeing memory inside that window drove the reading down and looked like a stale sampler. The growth pair is now sampled against a quiet child process, which also exercises the arbitrary-pid path the sampler exists for (#7926).
