Changed

- statusline savings segment renders `💸<session>%/<avg>%` instead of `💸<session>% (avg <avg>%)`
- `tm compress` is now a savings producer: a run that shrinks its input appends one `compress` row to `usage/savings.jsonl`, so the `💸` segment includes bash and gate output alongside instruction folding and bulk-read diversion. A passthrough run — output under the size gate, returned unchanged — writes no row, so a zero-saving run cannot drag the average down. The row's `basis` names the `compression_path` (`rtk_binary` / `native_fallback`) the run took.
