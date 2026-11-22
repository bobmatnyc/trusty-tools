Fixed
- `tm compress` records a zero-saving run's declined savings row as one line naming the reason `no_gain` in a day file, `usage/compress-declines/<YYYY-MM-DD>.jsonl`, under the trusty-mpm root, capped per day like the opportunity ledger. Before, it was a `debug` log the `info` filter hid. Nothing is added to `tm compress` stderr.
- The opportunity ledger records a Bash call that went through `tm compress` but saved nothing as `skip_reason: "compressed_no_gain"`; a call that saved keeps `"compressed"`. `tm savings status` lists the two reasons apart.
- `tm statusline` puts the `💸` savings segment second, after the version, so an 80-column pane no longer cuts it off.
