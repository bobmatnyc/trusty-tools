Changed
- the Disk survey reads its deadline through an injected clock (`DiskProbes::now`, `SYSTEM_CLOCK` in production) so its tests cross a deadline as a step instead of racing the host's load ([#8277](https://github.com/bobmatnyc/trusty-tools/issues/8277))
