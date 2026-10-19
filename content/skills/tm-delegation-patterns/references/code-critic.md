# tm-delegation-patterns — code-critic dispatch

## code-critic Dispatch Standard

The critic tier keys off the project's test-ladder rung (this repo's Rust Test
Ladder in `CLAUDE.md`) — never a parallel risk axis invented for the decision.

| Rung | Change class | Dispatch code-critic? |
|---|---|---|
| 1–2 | Docs, comments, changelog, test-only stabilization | Never |
| 3 | Localized behavior inside one crate | No — the PM reviews the diff |
| 4 | Cross-crate, public API, shared library | Only if a contract changes. Mechanical propagation does not qualify |
| 5–6 | Cross-crate contract, persistence, security, process lifecycle, release tooling, UI/API surface | Required |

Enum changes and spelling fixes are rung 1–3. No critic.

**Escalate to required regardless of rung:** the change can start, refuse, or
gate a session; it touches a trust boundary or an injection defense; or it
rewrites history or force-pushes.

**A critic round counts against the round limit.** After the first review, a PR
gets at most one fix round and one delta review. A blocking finding left after
that goes to the Architect, never to another critic round. Only findings in the
fix bar's blocking classes earn the fix round (`tm-workflow`, "Completion
Standard").

**A code-critic brief states the PR and the gates (#8584).** It carries
`PR: <n>|none`; with `none` the critic posts nothing and looks up no PR. It also
says the gates are read-only for the critic: it cites the engineer's reported
gate output and does not re-run one.

**Not a reason to dispatch:** a design question (send it to the owner, or the PM
decides), the PM wanting a second opinion, or confirming green CI.
