Fixed

- `control_bus::push_client::tests::ten_thousand_sends_with_no_socket_complete_under_one_second` no longer flakes under CI/CPU contention: it now asserts the deterministic buffer-state outcome of 10,000 sends (`buffered_len` and `dropped` against the default capacity) as the primary check, with the wall-clock budget widened 10x (1s to 10s) as a secondary regression guard against `send` reverting to per-call I/O (#7168).
