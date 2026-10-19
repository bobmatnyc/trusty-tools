# tm-delegation-patterns — briefs and acceptance criteria

## What a Brief Carries — and What It Must Not

The resident rule is in the instruction package: a brief carries findings,
evidence and constraints, not the implementation mechanism. This is the evidence
behind it.

In one session on 2026-08-15 a PM prescribed mechanisms in five briefs while
reasoning from DESCRIPTIONS of code rather than the code. An agent that opened
the source corrected it every time, and every correction was right. Read the
table as a dated record of five wrong prescriptions, never as a statement of how
any of this code behaves today.

| # | What the PM prescribed | Why the agent was right to overrule it |
|---|---|---|
| 1 | Relayed a critic's fix verbatim — "POST the rewritten input to the shared-tree-dispatch route, or upsert via `observe`" | Both were no-ops on that code. The route re-derived eligibility for itself, which an isolated input made false, and the observe path returned early on an id it already held |
| 2 | "Classify every commit segment" to close a compound-command bypass | Insufficient. `git add -A && git commit -m docs` has exactly one commit segment and the same hole. The engineer's rule — every segment must be a `cd` or THE one commit — was strictly stronger |
| 3 | "Both arrival orders converge" as the acceptance criterion for a new mutex | The test passed sequentially on one thread. Deleting the mutex left it passing |
| 4 | Told an agent a hygiene routine did a hard reset | The PM had read a comment that no longer matched its function. The agent opened the file and found a different operation |
| 5 | Detect a scratch daemon by comparing its data dir to the default | Cannot work. A scratch daemon used the DEFAULT FORMULA (`$HOME/.trusty-mpm`); only the `$HOME` it resolved against changed |

Cases 1, 2 and 5 were not answerable from any description — the answer was in
control flow. Case 4 is the argument against copying code behaviour into a brief
at all: the PM was wrong BECAUSE it trusted a written description.

The same five briefs were valuable everywhere they carried findings, evidence
and constraints — an ADR's own "it would matter if the grant were ever made
conditional", a prior false-deny incident, the Fail-Open Check. Those helped
every time. Only the prescriptions cost rounds.

**Relaying a reviewer's fix.** A `code-analyzer` or `code-critic` finding names a
real problem and usually names a remedy too. Relay the problem as a finding to
close; relay the remedy as a suggestion to VERIFY. Case 1 was a critic reasoning
from source and still wrong about the fix.

## Acceptance Criteria a Wrong Implementation Fails

Before stating a criterion, ask: what implementation would pass this and still be
wrong? Name one and the criterion does not test what you think it tests.

Case 3 is the worked example. "Both arrival orders converge" is satisfied by two
sequential calls, so it could not detect the missing mutex it existed to prove.
The criterion that worked: two threads, same `tool_use_id`, N rounds, exactly one
record each round.

| Weak criterion | What passes it while wrong | Stronger |
|---|---|---|
| "Both arrival orders converge" | Sequential calls on one thread, no mutex | Two threads, same key, N rounds, exactly one record per round |
| "The bypass is closed" | A guard that closes only the command you named | Name the class: every segment must be a `cd` or THE one commit |
| "Tests pass" | A change that added no test able to fail | One regression test that provably FAILED against the pre-fix commit |
| "23 criteria pass" on a `count` gate flipping 0 → 1 | Nobody reads the first plan's computed attribute values; a duplicated resource name ships because the count was never 1 before | Read the first plan's computed attribute values — names in particular — for any first-time-created resource, not only the count (#8130) |

Row three is the Fail-Open Check the instruction package already puts in
`code-analyzer` / `code-critic` briefs, applied to your own criteria.

## Architecture Suggestions From Agents

Agents surface improvement opportunities as part of their normal reporting. Cap
what reaches the user at **1-2 per session**, keep each specific rather than
vague, and ask before implementing any of them:

```
[Agent] found [issue]. Consider: [fix] -- [benefit]. Effort: [S/M/L]. Implement?
```

## Delegation Best Practices

1. **Provide context** — the findings, evidence and constraints the agent needs,
   not just the task, and not a chosen implementation mechanism (see "What a
   Brief Carries" above).
2. **Clear acceptance criteria** — how the agent knows it's done, each one
   written so a wrong implementation fails it (see "Acceptance Criteria a Wrong
   Implementation Fails" above).
3. **Wait for completion** — don't interrupt an in-flight delegation.
4. **Collect evidence** — get specific artifacts back (see
   `tm-verification-protocols`).
5. **Track files immediately** — right after the agent returns (see
   `tm-git-file-tracking`).
6. **Chain verification** — QA after implementation, always (CB#8).
