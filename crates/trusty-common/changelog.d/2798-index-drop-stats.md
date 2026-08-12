Added

- `search_index::index_drop_stats` (and `IndexDropStats`) report how many
  incremental index batches this process has dropped and how long ago the last
  one was ([#2798](https://github.com/bobmatnyc/trusty-tools/issues/2798)), so
  a saturation episode is readable state rather than only a `warn!` line.
  `dropped_batches == 0` means it has never happened; `seconds_since_last_drop`
  is `None` until the first drop. trusty-code's `GET /health` publishes both.
