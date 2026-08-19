Fixed

- `GET /status` no longer reports an in-flight count that never comes down. `POST /review` now holds the counter through the new `InFlightCountGuard`, whose `Drop` runs the decrement, so a client disconnect that drops the handler future mid-review releases the slot — as does a panic. The decrement saturates at zero rather than wrapping. See #5020.
