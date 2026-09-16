Fixed

- `tm wait`'s printed rerun command now echoes the slice, timeout and interval the run actually used instead of a hard-coded `--slice 100`. A caller who passed `--slice 540` is told to re-run with the clamped `--slice 500` the invocation really used, so re-issuing the line reproduces the wait rather than silently restarting it at the default budget (refs [#8120](https://github.com/bobmatnyc/trusty-tools/issues/8120))
