# PID forwarding through sidecar restarts

## Why

Issue #6967: the PID forwarder in `service/embedder_supervisor/mod.rs` exits
when the supervised PID becomes zero. The supervisor clears that slot during
restart backoff and later stores the replacement PID. The public health slot
therefore stays zero, hiding a healthy replacement and preventing the hardware
crash-budget integration test from targeting the next child.

## What

Keep forwarding zero and replacement PIDs for the current `SpawnedState`.
Identify that state by its shared PID-slot allocation under the existing state
mutex. Publish under the same guard, exiting when the state disappears or is
replaced, or after its definitive termination signal. Capture the state weakly
and release temporary strong references before sleeping. Preserve cancellation
of the previous task when a new lazy spawn starts (#829).

Clear public PIDs during explicit and idle shutdown under that same mutex,
before releasing the spawn gate. No public API, supervisor retry budget,
backoff, or hardware-test deadline changes are needed.

## Test

Use the existing shell stdio fixture as a real supervised child. Kill only the
PID captured from this test's current spawn, observe the actual/public zero
restart gap, then require the replacement PID to reach the public slot. Run
this regression before the fix and retain the failing result; always shut down
the owned supervisor before asserting a timeout result.

Also verify forwarder termination on shutdown and permanent give-up, then a
fresh lazy spawn without an old forwarder overwriting its PID. Run focused
lifecycle tests, crate check/clippy/fmt and the full crate suite with
`--no-fail-fast`; include ignored lifecycle coverage, especially the unchanged
real-hardware crash-budget test. Rung 5 also requires workspace check and direct
consumer coverage. Independent code review and security review precede delivery.
