Added

- **`TRUSTY_HNSW_DEMOTE_COOLDOWN_SECS`** — seconds of write-idle before a written HNSW store is persisted and demoted back to an mmap view. Default `300`; `0`, `off`, `false`, `no`, `disabled`, or `none` disables it; an unparseable value warns and falls back to the default. `TRUSTY_HNSW_REVIEW_IDLE` remains the kill switch for heap→view demotion as a mechanism and disables this path too; this knob turns off the write-cooldown path alone ([#6826](https://github.com/bobmatnyc/trusty-tools/issues/6826))
