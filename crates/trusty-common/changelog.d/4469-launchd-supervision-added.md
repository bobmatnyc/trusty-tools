Added

- **`trusty_common::supervision::launchd_supervision()` answers in THREE states**, because "launchd does not run this PID" and "launchd could not be asked" have opposite consequences and must never be conflated. An unspawnable `launchctl`, a non-zero exit, or a table with no parseable rows reports `Unknown` — never a confident `Supervised`. `is_launchd_supervised()` is kept as the boolean adapter for the post-upgrade self-restart decision and maps `Unknown` to `false`, the conservative direction there ([#4469](https://github.com/bobmatnyc/trusty-tools/issues/4469)).

- **The `launchctl` query is bounded by a 5-second timeout.** It runs on the daemon-start path, so an unbounded wait would let a wedged or saturated launchd hang startup indefinitely — an availability bug introduced by a diagnostic. The child is killed and reaped on expiry, and the timeout surfaces as `Unknown`, which every consumer already handles ([#4469](https://github.com/bobmatnyc/trusty-tools/issues/4469)).
