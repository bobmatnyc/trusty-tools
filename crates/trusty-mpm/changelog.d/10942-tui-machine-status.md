Added
- `tm tui` fills the MACHINE STATUS cards (CPU, memory, disk, network) and their sparklines from `console.machine_status` and `console.history` over the console's Unix socket, with no HTTP (#10942).
- The SERVICES table's `%CPU`, `MEM` and two 10-minute sparklines come from the same `console.history` answer's per-service samples (#10943).
- A console that is absent, slow, erroring or answering an unknown schema or variant shows a labelled state (`console absent`, `timeout`, `error`, `unknown`) instead of numbers, and a later failure replaces earlier data (#10942).
