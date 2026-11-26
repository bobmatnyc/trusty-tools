Fixed
- The `tm` bin test target gives every test thread an 8 MiB stack, so a debug-build parse of the full CLI no longer overflows the 2 MiB libtest default as verbs are added (#11926).
