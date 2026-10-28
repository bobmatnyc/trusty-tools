Changed
- `tm wait` now says plainly when a `pending` result means the `--slice` ended and not the `--timeout`: the status line carries a `note=` naming the slice, the remaining budget and the re-run options, and the `--slice` / `--timeout` help states that `--timeout` is the total budget across re-runs. Exit code 75 and the default slice of 100 are unchanged.
