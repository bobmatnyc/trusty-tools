Fixed
- Every non-loopback listener is served behind the tailnet peer gate, chosen by IP rather than list position. An Explicit `--http <non-loopback>` or `TRUSTY_CONSOLE_BIND=<non-loopback>` listener no longer serves every route ungated, and a wildcard (`0.0.0.0`) listener refuses every request (Refs #7524).
