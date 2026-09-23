Fixed
- The builder-capacity memory reading treats `sysinfo` reporting 0 bytes available (with a non-zero total) as unavailable, the same as a 0 total. It fails closed to the fixed ceiling on the `builder-cap-memory-read-failure` surface instead of reading 0 as a measurement, and `live_readings_are_plausible_on_this_host` no longer panics on a host where sysinfo reports 0 available (#8410).
