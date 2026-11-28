Fixed
- The `inbox-check` handler tests no longer read the test binary's stdin. Under a gate whose stdin is a pipe that stays open, the read never returned, the test held the crate's env lock, and every later env-locked lib test waited behind it until the run was killed (#11949).
