Added

- `tm-delegation-patterns` now carries two concurrent-dispatch rules: every brief must name the files owned by other in-flight branches ("we stack, we do not race"), and a running agent's scope is fixed — new work is a new agent, because cost tracks accumulated context over the agent's lifetime ([#4837](https://github.com/bobmatnyc/trusty-tools/issues/4837))
- `BASE-AGENT.md` bans ending a gate chain in a pipe — `cargo test … | tail` exits 0 on a failing suite — and gives the canonical redirect-then-echo form
