Fixed

- Serial `index_file` calls no longer trigger a full symbol-graph rebuild per call when each embed takes longer than the 2 s quiet window. A write now counts as active from its start, embed included, and the quiet rebuild waits for it; the 60 s max-wait rebuild is unchanged ([#10159](https://github.com/bobmatnyc/trusty-tools/issues/10159))
- After the graph-refresh ticker runs a deferred rebuild, the daemon returns freed heap to the OS (`malloc_trim` on Linux) and logs `graph-refresh: heap trimmed after deferred rebuild (#10159)` with `rss_before_mb` and `rss_after_mb` at info ([#10159](https://github.com/bobmatnyc/trusty-tools/issues/10159))
