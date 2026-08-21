Changed

- `events::recv_with_lag` now returns `Result<Result<HarnessEvent, Lag>, BusClosed>`
  instead of `Result<Result<HarnessEvent, Lag>, ()>`. `BusClosed` is a new public
  unit error type deriving `thiserror::Error`; it renders as
  "event channel closed: all senders dropped and the buffer is drained" and is
  re-exported from `events`. Callers that only tested `is_err()` are unaffected;
  callers that matched `Err(())` must match `Err(BusClosed)`. The `()` error tripped
  `clippy::result_unit_err` once CI's floating `dtolnay/rust-toolchain@stable` rolled
  to 1.98.0, turning the required workspace Clippy job red on every PR.
