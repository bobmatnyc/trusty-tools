Changed
- The host-metric window is sampled every second instead of every five:
  `HOST_SAMPLE_INTERVAL_SECS` is `1` and `HOST_HISTORY_CAPACITY` is `600`. The
  span it covers is the same ten minutes; the console's home-page cards draw a
  bar per second, which a five-second sample cannot render
  ([#6642](https://github.com/bobmatnyc/trusty-tools/issues/6642)).
