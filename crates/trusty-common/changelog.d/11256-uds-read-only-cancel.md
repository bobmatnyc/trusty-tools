Fixed
- `uds::server`: a method marked with the new `RpcRouter::mark_read_only` is cancelled when its client hangs up, and `handle_connection` reports `RpcServerError::ClientGone` instead of running the abandoned request to completion. Unmarked methods, including every write, still run to completion (#11256).
