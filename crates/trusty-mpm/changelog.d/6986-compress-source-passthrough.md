Fixed

- `tm compress` passes a source-file read through untouched. Reading
  `crates/x/src/tests.rs` with `cat` used to come back as zero bytes
  (`bytes_before=6037 bytes_after=0 pct_reduction=100.0
  compression_path=native_fallback`) because the path, not the program, chose
  the filter; a sibling `mod.rs` came back with its doc comments stripped. The
  classification fix lands in `trusty-agents-common`'s shared compression
  module, which `tm compress` calls (#6986).
