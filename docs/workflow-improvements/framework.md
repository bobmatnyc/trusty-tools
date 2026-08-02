# Framework workflow improvements

## Recommendation

The growth in issues is not explained only by increased feature work. Real defects are being found, especially in session lifecycle and test isolation, but the framework also encourages agents to turn review observations, QA side-findings, speculative risks, and implementation slices into durable GitHub issues too readily.

Change the framework from **ticket-driven for every discovered item** to **outcome-driven with explicit promotion criteria**:

- Use session tasks/checklists for work that belongs to the current outcome.
- Use a PR comment or parent-issue checklist for a non-blocking finding that has not crossed the ticket threshold.
- Create a standalone issue only when the item represents an independently prioritizable outcome.
- Keep implementation, required tests, documentation, and in-scope review fixes in the same PR.
- Scale test and review evidence to risk; do not require a full ceremony stack for every narrow change.

This is a policy correction, not a backlog purge recommendation.

## Evidence reviewed

Window: **2026-07-02 through 2026-08-02 inclusive**. August 2 is a partial day. GitHub search supplied issue and PR counts; `origin/main` at `150d0d2e` supplied landed-commit and changed-path data.

| Measure | Result | Interpretation |
|---|---:|---|
| New issues | 1,655 | 51.7/day over the 32-day inclusive window |
| New PRs | 1,072 | 33.5/day |
| Merged PRs | 1,044 | Delivery remained extremely high |
| New issues still open at cutoff | 490 | 29.6% of the month's intake |
| Issues labeled `trusty-mpm` | 1,005 | At least 60.7% of intake was framework/session-managed |
| Commits on `origin/main` | 1,044 | Matches the merged-PR count, consistent with squash-per-PR delivery |
| PR bodies containing `closes #` | 647 | At least 60.4% explicitly close an issue |
| New issue titles containing `test` | 123 | Testing is a material source of ticket intake |
| New issue titles containing `flaky` | 33 | Flake discovery is real, not merely perceived |
| New PR titles containing `test` | 67 | Significant delivery capacity went to test-focused work |
| Issue bodies containing `not confirmed` | 42 | Some durable tickets were created before confirmation |
| Issue bodies containing `optional` | 115 | Optional observations are often promoted into the tracker |
| Issue bodies containing `found during review` | 24 | Review is a direct source of additional tickets |

The weekly issue/PR relationship worsened as the month progressed:

| Created | Issues | PRs | Issues per PR |
|---|---:|---:|---:|
| Jul 2–8 | 210 | 158 | 1.33 |
| Jul 9–15 | 280 | 215 | 1.30 |
| Jul 16–22 | 550 | 379 | 1.45 |
| Jul 23–29 | 457 | 234 | 1.95 |
| Jul 30–Aug 2 | 358 | 86 | 4.16 |

The final row is only four days, but it is the clearest confirmation of the concern: issue creation accelerated while PR creation slowed.

### What is real

Several tickets describe separate, user-visible failure modes and should remain separate:

- [#1913](https://github.com/bobmatnyc/trusty-tools/issues/1913): a managed-spawn route skipped session preparation.
- [#1916](https://github.com/bobmatnyc/trusty-tools/issues/1916): a start route wrote artifacts into the source checkout.
- [#1918](https://github.com/bobmatnyc/trusty-tools/issues/1918): orphan GC could terminate a legitimate session.
- [#3569](https://github.com/bobmatnyc/trusty-tools/issues/3569) records an audit in which about 10.5% of sampled Test jobs failed and 43% of failures mapped to known isolation defects.

Those have distinct impact, acceptance criteria, or prioritization. Collapsing them would hide actual risk.

### What is workflow-amplified

- The same `trusty-console` flaky test received at least three tickets: [#2238](https://github.com/bobmatnyc/trusty-tools/issues/2238), [#2331](https://github.com/bobmatnyc/trusty-tools/issues/2331), and [#2634](https://github.com/bobmatnyc/trusty-tools/issues/2634). Later observations should have updated one canonical defect.
- [#1914](https://github.com/bobmatnyc/trusty-tools/issues/1914) explicitly says the suspected PATH problem was not confirmed as an active bug. That is a useful finding, but initially belongs on the parent issue/PR until reproduced or independently prioritized.
- [#3359](https://github.com/bobmatnyc/trusty-tools/issues/3359) was created after an APPROVE verdict for three non-correctness UX papercuts. Grouping the three is good; automatically creating a durable issue after an approving review should require an explicit value/priority decision.
- [#3691](https://github.com/bobmatnyc/trusty-tools/issues/3691) correctly groups two related deferred findings from one security fix. This is a better granularity pattern than one ticket per finding.
- One landed PR fixed five non-hermetic fixtures together ([commit/PR #4482](https://github.com/bobmatnyc/trusty-tools/pull/4482)), showing that the implementation boundary can be broader than the tickets created during separate gate failures.

## Framework instruction changes

### 1. Add a formal ticket-promotion gate

Place this in the non-overridable workflow rules and elaborate it in `tm-ticketing`:

> **A finding is not automatically a ticket.** Create a standalone issue only when it is an independently prioritizable outcome and at least one of these is true: (a) it is a reproduced user-visible defect; (b) it is accepted feature work; (c) it has a different owner, release, dependency, or security disposition from the current outcome; (d) it cannot reasonably fit the current PR without changing that PR's outcome or risk; or (e) the user explicitly requests tracking. Otherwise keep it as a session task, PR review item, or checklist/comment on the parent issue.

Require a duplicate search before creation. If a canonical issue exists, add the new reproduction/evidence there.

### 2. Add confidence and disposition to every finding

Use four confidence states:

| State | Meaning | Default disposition |
|---|---|---|
| Observed | User-visible behavior directly seen | Ticket if independently actionable |
| Reproduced | Repeatable with recorded steps/test | Ticket if independently actionable |
| Inferred | Code evidence supports the risk, no reproduction | Parent issue/PR note unless high-severity |
| Speculative | Plausible concern or analogy only | Session note; no ticket |

The reporting agent must label the state. Phrases such as “not confirmed,” “possible,” and “same risk class” should mechanically prevent automatic standalone issue creation unless severity justifies escalation.

### 3. Define issue granularity around outcomes, not findings

Add these rules:

- One issue may contain multiple symptoms with one root cause, owner, and acceptance test.
- Do not create separate issues for implementation, unit tests, integration tests, documentation, changelog, or review cleanup required to finish the same outcome.
- Split only when the work can be prioritized, shipped, reverted, or accepted independently.
- A follow-up is not a category that bypasses the promotion gate.
- Experiments are session-local until the project decides to adopt their result.
- Maintain one canonical issue/epic for a recurring flaky test or failure family; append occurrences rather than duplicating it.

### 4. Add a three-way review-finding disposition

Every code critic/QA finding should end in exactly one state:

1. **Fix in this PR** — correctness, security, acceptance criteria, regression coverage, or a small in-scope repair.
2. **Track under the parent** — useful but not independently prioritized; add a parent checklist/comment.
3. **Create a new issue** — crosses the promotion gate and includes independent acceptance criteria.

An APPROVE verdict must not automatically generate tickets for LOW/MEDIUM polish. The reviewer may recommend promotion, but the PM or user makes the prioritization decision.

### 5. Resolve contradictory gate language

The framework currently says both “no critic round on narrow changes” during SPRINT and that Code Analysis and QA are mandatory. Make the risk matrix authoritative:

| Risk | Required before PR | Required before merge |
|---|---|---|
| Low: docs, comments, mechanical metadata | Direct inspection or relevant static check | CI-required checks only |
| Normal: localized behavior change | Targeted regression + affected package checks | Affected-package suite; review may be lightweight |
| High: security, destructive paths, persistence, release/SemVer, cross-package contract | Targeted regression + affected/dependent suites + adversarial review | Full applicable hardening gates and explicit evidence |

“Mandatory QA” should mean that required evidence exists, not that a separate agent, phase, issue, or PR must always be created.

### 6. Replace raw-output requirements with concise evidence

The framework currently asks agents to “show raw test output.” For routine passing gates, require:

- exact command;
- pass/fail/ignored counts and duration when available;
- scope (targeted, package, dependent packages, workspace);
- any baseline/pre-existing failure classification;
- a link to CI or an attached log when full raw output matters.

Raw output remains required for a failure, disputed result, or explicit audit. This preserves verifiability without making PR bodies and ticket comments logs-by-default.

### 7. Clarify what belongs in one PR

Add this invariant to `tm-pr-workflow`:

> A PR contains one primary outcome and everything required to make that outcome safely shippable: implementation, regression tests, necessary refactoring, documentation/API updates, changelog fragment, and in-scope review fixes. Do not split these artifacts into separate PRs merely because different agents produced them. Split only when outcomes can be reviewed, deployed, or reverted independently, or when risk/size makes stacking materially safer.

Allow one PR to close multiple related tickets when one coherent change satisfies them. This is preferable to several coupled PRs with artificial ordering.

## Suggested issue and PR schemas

### Minimal issue

1. Outcome/problem and impact.
2. Confidence: Observed, Reproduced, Inferred, or Speculative.
3. Evidence/reproduction, concise and sufficient.
4. Acceptance criteria.
5. Relationship to parent work and duplicate-search result.
6. Test level expected for closure.

### Minimal PR

1. Primary outcome and linked issue(s).
2. What changed and what is intentionally out of scope.
3. Risk/blast radius.
4. Test evidence at the applicable levels.
5. Baseline/pre-existing failures and their canonical issue.
6. Documentation/changelog status.
7. Review-finding disposition: fixed here, kept on parent, or separately ticketed.

## Measure the policy for two weeks

Track these without setting a hard WIP cap:

- issues created per merged PR;
- duplicate issues found within seven days;
- issues created from APPROVE reviews;
- percentage of new issues marked Inferred/Speculative;
- median issues closed per PR;
- flaky-test observations appended to canonical issues versus new issues created;
- elapsed time from PR open to merge by risk tier.

Success is fewer durable artifacts per delivered outcome without reduced defect capture or weaker high-risk verification.
