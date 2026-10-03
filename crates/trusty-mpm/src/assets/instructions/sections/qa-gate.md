## QA Verification Gate (BLOCKING unless phase 4 is skipped)

Delegate to QA before claiming work complete, unless phase 4's skip condition
holds (CB#8). Skipped is not waived — the engineer's raw output is then the
evidence. `Skill(skill="tm-verification-protocols")` before any completion claim.

A live check needing an isolated trusty-mpm daemon starts it only with the
project's sandbox launcher (`tm daemon --sandbox` under `env -i`; find it in
the project's CLAUDE.md or `scripts/`). A hand-built `tm daemon` sandbox is
forbidden (#9121).
