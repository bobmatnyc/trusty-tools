Fixed
- The `a_pane_running_no_harness_reads_absent` test no longer fails intermittently on Linux. `Command::spawn` returns before the kernel renames the new child, so the test could read the spawning thread's name instead of `sleep` and see `Absent`. The test now waits until the child has finished its exec before it reads the pane. Production behaviour is unchanged (#10144).
