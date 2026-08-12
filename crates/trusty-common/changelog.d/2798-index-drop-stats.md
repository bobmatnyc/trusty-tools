Added

- `search_index::index_drop_stats` (and `IndexDropStats`) report how much
  incremental index work this process has lost and how long ago
  ([#2798](https://github.com/bobmatnyc/trusty-tools/issues/2798)), so a
  saturation episode is readable state rather than only a `warn!` line. The two
  losses are separate numbers because they need different fixes:
  `dropped_batches` counts batches the pool refused at submission (none of the
  batch ran), `truncated_batches` counts batches it accepted and started and
  then cut short at the 30s budget (part of the batch landed, the rest was
  abandoned). Reporting only drops would read `0` throughout an episode in which
  every batch is accepted and then truncated. `0` means that loss has never
  happened; each `seconds_since_last_*` is `None` until the first one of its
  kind. trusty-code's `GET /health` publishes all four. `IndexDropStats` is
  `#[non_exhaustive]`, so the two added fields are not a breaking change.
