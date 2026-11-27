Added
- `trusty-architect-spec-drift-check`, the Rust port of the Architect's `spec_drift_check.py`, with the library function `spec_drift::run` taking a `Clock`. Eleven recordings of the frozen script, made with a frozen clock, replay clean against it through the new `examples/replay_port.rs` (#12028). The port's `-h` description omits the instance id the script's description ends with.
