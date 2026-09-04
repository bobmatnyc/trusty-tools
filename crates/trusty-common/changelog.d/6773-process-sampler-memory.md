Added
- `ProcessCpuSampler` now refreshes process memory alongside CPU and exposes
  `rss_bytes(pid)`, so one refresh per tick serves both figures for a set of
  tracked pids. `physical_footprint_bytes` is the byte-granular form of
  `physical_footprint_mb` on macOS, which now divides it down rather than
  reading `proc_pid_rusage` a second time (#6773).
