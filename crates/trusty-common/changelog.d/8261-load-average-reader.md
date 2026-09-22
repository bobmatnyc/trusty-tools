Added
- `load_average` module (feature `load-average`): the kernel's 1/5/15-minute load average from `getloadavg(3)` on macOS/BSD and `/proc/loadavg` on Linux, as a `Result` that never substitutes a guessed value for a failed reading. Builder admission needs a sustained saturation measure, which `host_metrics`' instantaneous `CpuMetrics::usage_pct` is not. Adds no crate to the lockfile (#8261).
