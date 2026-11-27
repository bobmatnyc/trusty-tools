Added
- `trusty-architect-spec-drift-check`, the Rust port of the Architect's `spec_drift_check.py`, with the library function `spec_drift::run` taking a `Clock`. Twelve recordings of the frozen script, made with a frozen clock, replay clean against it through the new `examples/replay_port.rs` (#12028).
