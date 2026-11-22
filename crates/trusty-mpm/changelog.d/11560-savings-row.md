Fixed
- `tm compress` logs a zero-saving run's declined savings row at `info`, naming the reason `no_gain`, instead of at `debug`, where the `info` filter hid it.
- The opportunity ledger records a Bash call that went through `tm compress` but saved nothing as `skip_reason: "compressed_no_gain"`; a call that saved keeps `"compressed"`. `tm savings status` lists the two reasons apart.
- `tm statusline` puts the `💸` savings segment second, after the version, so an 80-column pane no longer cuts it off.
