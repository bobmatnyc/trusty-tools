Fixed

- `tm compress`'s `pct_reduction` is now pinned by a test that asserts the
  emitted number on an eliding `native_fallback` compression, not just that
  logging did not panic. `log_compression_stats` returns the percentage it
  logs so the value is assertable end to end. The reported
  `bytes_before=3706 bytes_after=1709 pct_reduction=0.0` could not be
  reproduced — that call's durable record carries the correct
  `ratio=0.461` — and nothing in the code had ever verified the number, which
  is what left the claim unfalsifiable (#7180).
