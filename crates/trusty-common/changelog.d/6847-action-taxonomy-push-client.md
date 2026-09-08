Added

- `control_bus::HarnessPayload::Action`, carrying the six-kind `ActionEvent` taxonomy (`Workflow`/`Agent`/`File`/`Tool`/`Session`/`Inference`) and its supporting types (`ActionMeta`, `Actor`, `ObjectRef`, `ObjectType`, `PathRef`, and five phase enums) per DOC-73 §3.2. Additive: an old subscriber matching on `domain` alone skips an `action` frame cleanly.
- `control_bus::PushClient` (behind the `uds` feature, unix only): the buffered producer-side UDS transport to trusty-console's event-bus ingest socket (DOC-73 §4.2). `send` is synchronous and never blocks the producer; it enqueues into a bounded buffer (default capacity 4096) and drops the oldest frame on overflow, counting every drop. `flush` dials console and drains the buffer, stopping and re-queuing on the first failed frame.
