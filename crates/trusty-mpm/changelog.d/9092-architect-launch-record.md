Fixed
- `tm fleet status` prints a `WARNING` line naming the PID when the Architect session's live `claude` has no launch record, for example one started by a hand-typed `claude --resume`, and points at `tm fleet relaunch` (#9092).
- The managed-launch shim (`tm internal-spawn-disclaimed --launch-spec`) refuses a launch spec that names the supervisor (Architect) session and starts nothing; the Architect starts only through `tm fleet init` or `tm fleet relaunch`. A spec that names a session is also refused when the session store cannot be read. Other launches are unchanged (#9092).
