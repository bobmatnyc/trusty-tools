Fixed
- The `tm dbatg` fallback tests no longer overflow the 2 MiB test-thread stack: they parse only the `dbatg` arguments, and the one test that checks the whole `tm` CLI parses on an 8 MiB thread (CI run 44709752864).
