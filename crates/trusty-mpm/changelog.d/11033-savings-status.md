Added
- `tm savings status [--by tool|subagent] [--days N] [--json]` prints, per day and group, the tool-output tokens seen, compressed and saved, the share compressed, an upper-bound would-save figure and the skip-reason split; days with dropped rows are marked undercounted (#11033).
- `tm doctor` gains a `compress_rows_per_day` row that lists compress rows per day and warns when the latest complete day is more than 80% below the 14-day median (#11033).
