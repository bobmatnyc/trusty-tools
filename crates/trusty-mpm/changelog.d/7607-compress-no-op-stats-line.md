Fixed
- `tm compress` no longer writes its `bytes_before=… pct_reduction=0.0` stats line into the caller's tool result on a no-op run. The line is emitted only when the byte counts moved — a reduction, or an expansion — or when the wrapped command reported a failing exit status (#7607).
