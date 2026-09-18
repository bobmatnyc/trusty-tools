Changed
- `HostSampler::sample` now refreshes disk metrics on their own slower cadence (`DISK_SAMPLE_INTERVAL_SECS`, default 15 s) instead of every call, serving the cached `DiskMetrics` in between; CPU, memory and network keep the 1 s cadence. The first sample always refreshes. New `HostSampler::with_thresholds_and_disk_interval` sets the cadence explicitly.
