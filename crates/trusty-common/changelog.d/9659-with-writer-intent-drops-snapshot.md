Changed
- `PalaceRegistry::with_writer_intent()` on an already-hydrated registry now drops any read-only snapshot handle, logs an ERROR and records that palace unopenable, instead of keeping the snapshot (#9659).
