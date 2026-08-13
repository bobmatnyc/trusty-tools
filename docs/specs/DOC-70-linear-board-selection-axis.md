---
spec_refs: []
---

# DOC-70 — Linear Project-Board Selection as a Second Audit Selection Axis

**Status:** Draft. §12's six questions are decided here; §13's two are open and
need the owner. No code has been written and no issue in §14 has been filed.
**Spec ID:** `SPEC-BOARDAXIS-01~draft` … `SPEC-BOARDAXIS-14~draft`
**Subsystem:** `trusty-audit` — the pre-sweep selection surface, the `state/`
record, the Linear credential's lifetime; `trusty-common` — the one Linear
client the audit path consumes, extended with two workspace-scoped calls;
`trusty-git-analytics` (tga) — the board-listing subcommand the picker calls,
the pinned corpus, and the commit partition; `trusty-review` — the report
section that makes the axis visible, unchanged apart from that section
**Owner:** Bob Matsuoka
**Last-updated:** 2026-08-13
**DOC-N claim:** `DOC-70`, scan-before-claim per DOC-38 §4.1. Verified against
this worktree (branched from `origin/main` at `e892334f`):
`scripts/check_doc_numbers.sh` reports 120 docs / 114 claims, 3 grandfathered,
0 violations before this file was added; `docs/specs/README.md`'s own catalog
note (updated 2026-08-12) names `DOC-70` as the next free number after
DOC-69; a `grep` for a `DOC-70` self-label anywhere under `docs/specs/**`
found only that catalog note and DOC-69's own reference to it; a
`gh pr list --search` for `DOC-70` across open PRs in `bobmatnyc/trusty-tools`
returned one unrelated match (PR #5550, a trusty-search test PR that claims no
`DOC-N`).
**Builds on:** [DOC-68 — The Audit Engagement Handoff Package](./DOC-68-audit-handoff-package.md),
which commits repo selection to a `gh`-only flow (§6, §14 Q4), and
[DOC-67 — tga AUDIT Mode](./DOC-67-tga-audit-mode.md), whose one-shot
constraint (§2), continue-on-failure policy (§9), and Gaps & Caveats
convention (§8) this spec inherits unchanged. This is a **new spec, not an
amendment**: DOC-68 §6's sequence has one selection axis and no place to put a
second, and DOC-68 §4 explicitly defers the engagement-config schema to #5478
rather than growing to cover new fields.
**Related issues:** this spec's own issue
[#5641](https://github.com/bobmatnyc/trusty-tools/issues/5641); epics
[#5473](https://github.com/bobmatnyc/trusty-tools/issues/5473) (handoff
package) and [#5477](https://github.com/bobmatnyc/trusty-tools/issues/5477)
(the auditor client); hard dependencies
[#5219](https://github.com/bobmatnyc/trusty-tools/issues/5219) (JIRA,
GitHub Issues and Linear do not write to `work_items`) and
[#5405](https://github.com/bobmatnyc/trusty-tools/issues/5405) (the AUDIT
report never reads the board data it collects); the repo-axis issues this
composes with, [#5487](https://github.com/bobmatnyc/trusty-tools/issues/5487)
(repo discovery), [#5497](https://github.com/bobmatnyc/trusty-tools/issues/5497)
(pre-sweep picker) and [#5494](https://github.com/bobmatnyc/trusty-tools/issues/5494)
(state persistence + resume); [#5475](https://github.com/bobmatnyc/trusty-tools/issues/5475)
(`gh` common entry point), a prerequisite of the repo axis and not of this one.

---

## {#SPEC-BOARDAXIS-01~draft} 1. Purpose

A client company running the handoff package selects two things, not one:
which repositories to audit, and which Linear project board. The board is a
**selection axis that scopes what the audit examines**, on the same footing as
the repository set — not a source of enrichment data bolted onto a report that
was already fully determined by the repo choice.

That distinction is what this document exists to make precise. #5219 and #5405
describe a report that fails to read board data it already collected; fixing
either of them makes the report richer without changing what the audit is
about. Selecting a board changes what the audit is about: two runs over the
same repositories against two different boards produce different work-item
corpora, different commit partitions, and different delivery findings.

This spec answers what a board scopes, whether one is required, which of the
three existing Linear implementations the audit path consumes, where the
Linear API key lives, how the selection persists across a restart, and whether
any of it can ship before #5219 and #5405.

## {#SPEC-BOARDAXIS-02~draft} 2. Relationship to DOC-67 and DOC-68

DOC-68 §6 specifies repo selection end to end: detect `gh`, authenticate,
discover what the credential can reach, render a picker, then start the
unattended sweep. Its §14 Q4 routes both discovery and cloning through `gh`
rather than a second GitHub API client. Nothing in that sequence anticipates a
second axis, and nothing in it needs to change to accommodate one — the board
axis composes with it rather than replacing any part of it.

Three DOC-67 constraints carry over unchanged and are not restated as new
decisions here:

- **One-shot (DOC-67 §2).** Board selection is pre-sweep interaction, in the
  same phase as `gh auth login` and the repo picker. Nothing after the sweep
  starts prompts for a board, a key, or a confirmation.
- **Continue on failure (DOC-67 §9).** A Linear query that fails does not
  abort the run. It produces a named gap, the same way a failed clone does
  under DOC-68 §8.
- **Gaps over blanks (DOC-67 §8).** A board-derived section with no data says
  so in Gaps & Caveats. It never renders empty, and it never renders a zero
  that a reader would take for a measurement.

## {#SPEC-BOARDAXIS-03~draft} 3. What a Board Scopes — the Testable Definition

Two axes, and each decides a different question. The repo axis decides which
**code** is examined. The board axis decides which **tracked work** is in
scope, and how the examined commits are partitioned against it.

**The audited repository set** is exactly the set selected on the repo axis
(DOC-68 §6). A board never adds a repository and never removes one.

**The time window** is `SweepOptions.weeks`
(`crates/trusty-git-analytics/src/audit/sweep.rs:52-53`), the operator input
that already exists. A board never sets it and never overrides it.

**The work-item corpus** is what the board decides first. Call it `B`: every
Linear issue whose project is one of the selected projects, resolved once
during the pre-sweep phase. The run's `work_items` rows with
`source = 'linear'` (`crates/trusty-git-analytics/src/core/db/sql/0005_work_items.sql:6-18`)
are exactly `B`. An issue outside `B` is not written, even when a collected
commit references it.

**The commit partition** is what the board decides second. For each commit
inside the audited repositories and the window,
`LinearClient::extract_issue_ids`
(`crates/trusty-git-analytics/src/collect/linear/client.rs:162`) yields zero or
more identifiers from the commit message. The commit is **board-linked** when
at least one of them names an issue in `B`, and **unlinked** otherwise. Both
partitions are collected, analyzed, and reported. Neither is discarded, and no
commit is excluded from the code analysis because of which partition it fell
into.

A reader can therefore answer the scope question for any input without running
anything:

| Question | Answer |
|---|---|
| Is repository `R` audited? | Only if `R` was selected on the repo axis. The board is irrelevant. |
| Is commit `c` examined? | Only if `c` is in an audited repository and inside the window. The board is irrelevant. |
| Is commit `c` board-linked? | Only if an identifier in its message resolves to an issue whose project is a selected project. |
| Is issue `i` in the corpus? | Only if `i`'s project is a selected project, as of the pre-sweep resolution. |

The board scopes the audit's view of tracked work, never its view of code.
That is a real scoping decision with a visible effect on the deliverable, and
it cannot quietly shrink the codebase an acquirer paid to have examined.

## {#SPEC-BOARDAXIS-04~draft} 4. Scope

**In scope for this spec:**
- The scoping definition above, at the grain a test can assert (§3).
- Whether a board is required, and what a run with repos and no board does
  (§5).
- Which Linear implementation the audit path consumes, and the seam it reaches
  it through (§6).
- The Linear credential's lifetime and the two places it must never reach
  (§7).
- The `state/` record's board block and its interaction with #5494's resume
  (§8).
- The dependency order against #5219 and #5405 (§10).

**Out of scope, and left where it already lives:**
- The repo axis itself — DOC-68 §6, unchanged.
- The engagement-config TOML schema — #5478's, and §7 decides it gains no new
  field.
- The sweep's internal behavior — DOC-67, unchanged.
- The report's board-derived section's exact copy and placement — #5405 owns
  the read side; §9 states what the section must carry, not how it is worded.
- Folding `crates/trusty-agents/src/ticketing/linear.rs` into the shared
  client (§6). It stays a third implementation; this spec adds no fourth.
- Non-Linear boards. §13's first open question is whether they are ever
  offered.

## {#SPEC-BOARDAXIS-05~draft} 5. Optional, and Chosen Explicitly

**A board is optional.** The repo axis alone already produces the whole DOC-67
report; every board-derived section is additive to it. Requiring a board would
block every engagement whose client runs JIRA, Azure DevOps, GitHub Issues, or
no tracker at all.

**The picker offers "no board" as a selection, not as a skip.** The state
record (§8) distinguishes "the operator selected no board" from "the field is
absent", so the report's gap line can say which. Those are different facts
about a run and a reader acts on them differently — one says the engagement
did not scope to a board, the other says something in the client failed to
record what happened.

**A run with repositories and no board** executes exactly the DOC-68 sequence.
No Linear query is made, no Linear credential is asked for, and every
board-derived section renders as a stated gap under DOC-67 §8's convention.

## {#SPEC-BOARDAXIS-06~draft} 6. Which Linear Client the Audit Path Consumes

Three unrelated Linear implementations exist today:

| Implementation | What it can do | Why it is or is not the audit's client |
|---|---|---|
| `crates/trusty-common/src/tickets/api/backends/linear/` | The `Backend` trait over Linear's GraphQL API, including `list_projects` and `get_project` (`backend.rs:365-398`), issue CRUD, labels, cycles | **This is the one.** It is the only implementation with a project-level surface, and the only one behind a trait with more than one backend. Both extensions §6 needs are additive to it. |
| `crates/trusty-git-analytics/src/collect/linear/` | Fetch one issue by identifier (`client.rs:81`), extract identifiers from a commit message (`client.rs:162`), persist to `linear_issues` (`client.rs:247`) | The narrowest of the three. It has no project surface and no way to enumerate anything — it only answers "what is `ENG-123`". Promoting it to the shared client means writing the one that already exists in trusty-common. |
| `crates/trusty-agents/src/ticketing/linear.rs` | A `TicketProvider` for the assistant surface — create, get, update, close, list, comment | A third implementation, and standing debt. Untouched here (§4). |

**The audit consumes trusty-common's, and reaches it through tga as a
subprocess.** `trusty-audit` does not link trusty-common. Its dependency set is
anyhow, clap, serde, toml, thiserror, trusty-installer and tokio
(`crates/trusty-audit/Cargo.toml:38-58`), and that manifest's own comment
records that `reqwest` is deliberately absent because downloading and HTTP
belong to `trusty-installer`. The picker calls a new `tga linear boards --json`
subcommand and reads its stdout — the same process-boundary seam DOC-68 §5
Seams 3 and 4 already use for the sweep and for `trusty-review report`.

**Consequence for the pre-sweep order.** The board list comes from the pinned
`tga` binary, so board selection happens after tool installation, not before
it. `Session::guided`'s chain (`crates/trusty-audit/src/session.rs:245-266`) is
SelectRepositories → InstallTools → ReadyForRun today; it gains a `SelectBoard`
step between InstallTools and ReadyForRun. All of it is still the one
interactive phase DOC-67 §2 permits.

**What moves, and what stays.** tga's `collect/linear/client.rs` transport —
its own `reqwest` client and its own `Authorization` header (`client.rs:60-71`,
`:101`) — folds onto trusty-common's `graphql` helper
(`crates/trusty-common/src/tickets/api/backends/linear/client.rs:36-58`). What
stays in tga is `extract_issue_ids`, the `linear_issues` persistence, and the
`ticket_regex` config (`crates/trusty-git-analytics/src/core/config/mod.rs:847-861`).
Those are commit analysis, which is tga's domain; the GraphQL transport is not.

**Two additive extensions to trusty-common, and what they cost:**

1. **Workspace-scoped project listing.** `list_projects` resolves a team first
   (`backends/linear/backend.rs:365-368`) and `resolve_team_id` errors when
   neither `team_key` nor `team_id` is configured (`backends/linear/client.rs:67-70`).
   The picker must show every board the key can reach, across teams. Added as a
   defaulted method on `Backend` (`backends/mod.rs:106`), this is not a
   breaking change for implementors.
2. **Paging a project's issues.** `list_issues` is team-scoped
   (`backend.rs:118-126`); the corpus needs the project-scoped query.

Costs, named rather than assumed:

- tga must enable trusty-common's `tickets` feature, which requires `mcp` and
  pulls `uuid`, `base64` and `toml` (`crates/trusty-common/Cargo.toml:505`).
  tga already depends on trusty-common with five features
  (`crates/trusty-git-analytics/Cargo.toml:48`), so this adds a feature, not an
  edge.
- `Project` (`crates/trusty-common/src/tickets/api/models.rs:170-181`) is not
  `#[non_exhaustive]`. If the picker needs a field it lacks, adding one is a
  breaking change that the release-time semver gate will catch, and for a
  `0.y.z` crate the bump is the MINOR position.
- Rewiring tga's Linear transport is a cross-crate change: rung 4 of the test
  ladder — `cargo check --workspace` plus `cargo test -p <consumer>` for each
  direct trusty-common dependent.

**A fourth implementation inside `trusty-audit` is a defect, not a
shortcut.** CLAUDE.md's common-entry-point rule forbids it, and the crate has
no HTTP client to build one on by deliberate design.

## {#SPEC-BOARDAXIS-07~draft} 7. Where the Linear API Key Lives

**The Linear key is the recipient's credential, not the owner's.** It never
enters `engagement.toml`, never enters `state/`, and never enters the return
package.

`EngagementConfig` carries the OpenRouter key because the owner mints that key
per engagement and it travels outbound
(`crates/trusty-audit/src/config.rs:1-24`, `:164-167`). The owner does not have
the client's Linear workspace credential when the package is built and must not
ask for it. The precedent that fits is the recipient's GitHub credential, which
DOC-68 §13 keeps in `gh`'s own local store and out of both files.

**Lifetime and handoff:**

- The client asks for the key during the pre-sweep phase and holds it in memory
  as a value with the same three properties `SecretKey` has
  (`crates/trusty-audit/src/config.rs:32-77`): no `Serialize`, so it cannot be
  written into an output artifact without a compile error; redacting `Debug`,
  so it cannot reach a tracing field; redacting `Display`, so it cannot reach
  an error string. Plaintext is reachable only through the greppable `expose`.
- It reaches the sweep as `LINEAR_API_KEY` in the `tga` child process's
  environment. That is the expansion path tga already supports
  (`expand_env_var`, `crates/trusty-git-analytics/src/collect/linear/client.rs:281`;
  `LinearConfig.api_key`'s own doc, `core/config/mod.rs:833-836`).
- It is never written into a generated tga `config.yaml` under the working
  directory. That file would put a plaintext credential on the recipient's disk
  with no deletion story beyond `rm -rf <work-dir>`, and it would route around
  the no-`Serialize` guarantee rather than honour it.

**The limit, stated rather than implied.** A child process's environment is
readable by other processes running as the same user on the recipient's
machine. That is weaker than the compile-time guarantee against the key
reaching an artifact, and it is what the subprocess seam costs. It does not
weaken the two properties the handoff depends on: the key does not reach disk,
and it does not reach the package that comes back.

## {#SPEC-BOARDAXIS-08~draft} 8. How the Selection Persists, and What Resume Must Pin

`Area::State` already describes itself as "repo selection and run progress"
(`crates/trusty-audit/src/workdir.rs:68`, `:104`). Board selection is
selection, so it lands in the same record — one file, one atomic write at the
end of the pre-sweep phase, no new area and no second write path that could
disagree with the first about whether that phase completed.

The record's board block holds, per selected board: the provider tag, the
board id, its name, and its URL. Plus the marker §5 requires, distinguishing an
explicit "no board" from an absent field. Plus **the resolved issue-identifier
list and the timestamp it was resolved at.**

**That last part is what #5494's resume turns on.** A Linear project's issue
set changes while a multi-hour sweep runs. If a resumed run re-queries the
board, repositories processed before the restart were partitioned against one
corpus and repositories processed after it against another, and the report's
board-linked and unlinked counts no longer sum to the commits examined. Resume
therefore reads the pinned list and does not re-query. A run that wants a fresh
corpus is a new run, not a resume — and #5494 must encode that, not merely cite
this spec.

**What the record does not hold: issue titles or bodies.** The pinned list is
`ENG-123`-shaped identifiers and Linear issue ids. Titles reach
`work_items.title` in the extract database and nowhere else, which DOC-67 §10
and DOC-68 §10 already cover in the attestation language ("no file content,
diffs, patches, hunks, or blobs", never "no code", because free-text columns
exist). Keeping titles out of `state/` means the board block adds no new
data-handling surface to DOC-68 §13's posture.

## {#SPEC-BOARDAXIS-09~draft} 9. What the Report Must Carry

The read side is #5405's. What this spec requires of it, and no more:

- The board or boards the run was scoped to, named in Report Metadata, so a
  reader knows which selection produced the numbers.
- The corpus size and the two partition counts, and enough for them to be
  checked against each other: commits examined, commits board-linked, commits
  unlinked.
- A detection-limits line beside the partition, in the shape DOC-67 §8 already
  uses for `agentic_pct`. Identifier extraction is regex-based over commit
  messages, so a squash merge that drops the identifier, or a house convention
  that never wrote one, produces an unlinked commit that was in fact tracked.
  A low board-linked share means "no identifiers found", not "work was
  untracked", and the section says so rather than leaving the reader to assume
  otherwise.
- When no board was selected, a Gaps & Caveats line saying that, per §5.

Whether the unlinked share may additionally be **graded** is open — §13.

## {#SPEC-BOARDAXIS-10~draft} 10. Dependency on #5219 and #5405 — Not Independent

**This spec depends on both, and the order is #5219, then #5405, then the
board axis.** Stated plainly because the alternative reading — that the
selection axis is separable and can ship first — produces a picker whose
selection changes nothing an acquirer can see.

**#5219 is a hard prerequisite for §3's corpus.** Linear collection today
writes `linear_issues` (`crates/trusty-git-analytics/src/collect/linear_pipeline.rs:31-45`,
`store_linear_issues` at `collect/linear/client.rs:247`) and never touches
`work_items`; #5219 records that only Azure DevOps writes `work_items` in
production, and that every `tga.db` on the owner's machine has `work_items=0`.
§3 defines the board's scope in terms of `work_items` rows with
`source = 'linear'`. Without #5219's writer, there are none.

**#5405 is a hard prerequisite for the axis being visible.** It records that
`src/report/` reads none of the four board tables. A selection that reaches
`work_items` and stops there changes the database and not the deliverable.

**Rejected: defining the corpus over `linear_issues` instead**, by adding a
project column to `crates/trusty-git-analytics/src/core/db/sql/0002_linear_issues.sql`.
It would work today with no dependency on #5219 at all. It is still the wrong
answer: it builds a second work-item store inside the crate that already has
one, and it leaves #5219 to solve the same problem again for JIRA and GitHub
Issues afterwards — with the Linear path now diverged from the one the fix
lands on.

## {#SPEC-BOARDAXIS-11~draft} 11. What Composes, and What Does Not

`work_items.source` is already `'azdo' | 'jira' | 'github' | 'linear'`
(`core/db/sql/0005_work_items.sql:8`), so §3's corpus definition does not
collide with the other three writers #5219 will add — a run scoped to a Linear
board writes Linear rows, and a client on JIRA is §13's first open question,
not a conflict.

`LinearConfig.team_keys` (`core/config/mod.rs:838-841`) already filters
collection by team key. It stays as it is. Team filtering is strictly coarser
than project scope and is not the new axis; a run that sets both gets the
intersection, which is what both settings independently mean.

`tga`'s Linear collection is driven by `fetch_on_reference`
(`collect/linear_pipeline.rs:37-38`) — it fetches only issues that a commit
message referenced. §3's corpus inverts that direction: the board names the
issues, and the commits are matched against them. Both directions coexist, and
the pinned corpus is the one that decides `work_items`.

## {#SPEC-BOARDAXIS-12~draft} 12. Decided Questions

Six questions #5641 asked this spec to decide. Each states the decision, its
rationale, and what was rejected.

**Q1 — What does a board scope? The work-item corpus and the commit
partition. Never the repository set, and never the time window.** §3 gives the
per-input test.

*Rationale:* it is the only definition that both changes what the audit
examines and fails visibly. A board-linked share of zero is a stated number a
reader can interrogate; an audit that silently examined zero repositories looks
identical to a completed one.

*Rejected — the board filters the repository set*, by intersecting the
selected repos with those the board's issues touch via Linear's GitHub
attachments or branch names. The intersection is only as complete as the
client's Linear–GitHub integration and their branch-naming discipline. A client
with the integration switched off yields an empty intersection, and the run
audits nothing while presenting as a finished audit. That is the failure class
DOC-67 §9 already rejects for an unreachable analyze daemon: silence reading as
a clean bill of health.

*Rejected — the board sets the time window* from the project's start and
target dates. Those are planning artifacts, routinely stale or absent, and the
window is already an explicit operator input. An implicit override would make
two runs with the same `--weeks` cover different periods.

*Rejected — the board restricts to a team's work items.* Strictly coarser than
project scope, and already reachable through `LinearConfig.team_keys` (§11).

**Q2 — Required or optional? Optional, and selected explicitly.** §5.

*Rationale:* the repo axis alone produces the whole DOC-67 report. Requiring a
board excludes every client on JIRA, Azure DevOps, GitHub Issues, or nothing.

*Rejected — required:* excludes those clients outright.

*Rejected — an implicit skip when no Linear key is configured:* it makes "the
engagement was not board-scoped" indistinguishable from "the client failed to
record the selection", which is exactly the distinction DOC-67 §8's Gaps &
Caveats convention exists to keep.

**Q3 — Which client? trusty-common's, reached through tga as a subprocess.**
§6.

*Rationale:* trusty-common's is the only one of the three with a project
surface, both extensions it needs are additive, and routing through tga keeps
`trusty-audit`'s dependency set and its deliberate absence of an HTTP client
intact. Consolidation is required first: tga's transport folds onto
trusty-common's GraphQL helper, keeping identifier extraction and
`linear_issues` persistence in tga.

*Rejected — a fourth client in `trusty-audit`:* forbidden by CLAUDE.md's
common-entry-point rule, and the crate has no HTTP client to build it on.

*Rejected — promoting tga's client to the shared one:* it is the narrowest of
the three, with no project surface and no trait, so promoting it means writing
what trusty-common already has.

*Rejected — `trusty-audit` linking trusty-common directly:* it works, and it
adds a large dependency edge plus a second HTTP client to the crate whose
manifest records that it deliberately has none. The subprocess seam already
exists.

**Q4 — Where does the key live? In memory during the pre-sweep phase, in the
`tga` child's environment during the sweep, and on disk nowhere.** §7.

*Rationale:* the key belongs to the client's workspace, not to the owner, so
the outbound-config precedent does not apply and the `gh`-credential precedent
does. Whatever type holds it carries `SecretKey`'s three properties, so the
compile-time guarantee against reaching an artifact extends to it.

*Rejected — a `linear_key` field in `EngagementConfig`:* the owner would have
to hold a client's workspace credential to write it, inverting the credential
asymmetry that module exists to enforce.

*Rejected — writing it into a generated tga `config.yaml`:* plaintext on the
recipient's disk, and a way around the no-`Serialize` guarantee rather than a
way to honour it.

*Rejected for v1 — an OS keychain:* viable, and the only way to avoid asking
again after a restart. It adds a platform-specific dependency to a client whose
delivery story is a self-contained zip, and asking again is legal because a
restart re-enters the pre-sweep phase, which is where DOC-67 §2 permits
interaction.

**Q5 — How does it persist? In the same `state/` record as repo selection, one
atomic write, with the resolved issue-identifier list pinned.** §8.

*Rationale for the pin:* a Linear project's issue set moves while a
multi-hour sweep runs, so a resumed run that re-queries partitions its
before-and-after repositories against different corpora and the report's counts
stop summing.

*Rejected — a separate `board.json`:* two files for one selection, and two
write paths that can disagree about whether the pre-sweep phase completed.

*Rejected — pinning nothing and re-querying on resume:* the counts problem
above.

*Rejected — pinning the corpus into the extract database:* `extract/` is the
sweep's artifact (`workdir.rs:66`), and writing it pre-sweep makes the
selection phase a producer of something the sweep owns.

**Q6 — Dependency on #5219 and #5405? Both are hard prerequisites, in that
order.** §10. Implementing the axis first produces a picker whose selection has
no observable effect.

## {#SPEC-BOARDAXIS-13~draft} 13. Open Questions for the Owner

Two questions this spec is not entitled to settle. Each states the options and
a recommendation.

**OQ1 — Is the board axis Linear-only, or is Linear the first provider of a
general board axis?**

It decides the `state/` record's shape and the subcommand's name — a
`linear_project_id` field and `tga linear boards`, or a `{provider, id}` pair
and `tga boards --provider linear`.

*Options:* (a) Linear-only, untagged. (b) Provider-tagged from the start, with
Linear the only implementation.

*Recommendation: (b).* The tagged shape costs one field now; the untagged one
costs a state-file migration later, in a client that persists across restarts.
tga already models four trackers in `work_items.source`, so the tagged shape
matches what the database already says. But whether JIRA or Azure DevOps boards
are ever offered is a decision about which clients the handoff targets, and
that is the owner's, not a shape this spec can infer.

**OQ2 — May the report grade the unlinked-commit share, or only report it?**

§3's partition makes "what fraction of shipped work was tracked on the board"
computable, and §9 requires it to be shown. Whether it may also become a
RED/AMBER/GREEN finding is a different question.

*Options:* (a) report the share with a detection-limits line and no grade.
(b) fold it into the scoring as a delivery-process finding.

*Recommendation: (a) for v1.* An acquirer reading AMBER for "untracked work"
takes it as evidence about the target's engineering discipline, and the
number's error bars are set by the target's commit-message conventions —
squash merges that drop identifiers, hotfix paths that never carried one,
mono-repo commits spanning several projects. DOC-67 §8 hit the same shape with
`agentic_pct` and resolved it by rendering the detection limits verbatim beside
the number rather than grading it (#5249, #5250). Whether the audit is entitled
to make the stronger claim is the report's credibility, which is the owner's
call and not a mechanism this spec can decide.

## {#SPEC-BOARDAXIS-14~draft} 14. Implementation Issues That Would Follow

Described, not filed. Nothing below exists as an issue.

1. **trusty-common: workspace-scoped Linear project listing.** A defaulted
   `Backend` method plus a Linear implementation that does not call
   `resolve_team_id`. Additive; a defaulted trait method is not breaking for
   implementors. Rung 4.
2. **trusty-common: page a project's issues.** The project-scoped query
   `list_issues`'s team-scoped one cannot express.
3. **tga: fold `collect/linear/client.rs`'s transport onto trusty-common's
   GraphQL helper.** Keeps `extract_issue_ids`, `linear_issues` persistence and
   `ticket_regex` in tga. Enables the `tickets` feature on tga's existing
   trusty-common dependency. Rung 4; depends on 1 and 2.
4. **tga: `tga linear boards --json`.** Lists the boards a key can reach,
   machine-readable, no TTY, non-zero exit on an auth failure so the picker can
   distinguish "no boards" from "the key is wrong". Depends on 1 and 3.
5. **tga: write `work_items` from a pinned Linear corpus.** This is #5219's
   Linear half; what is new is that the corpus arrives as a pinned list rather
   than being discovered from commit references.
6. **trusty-audit: a `Boards` command and a `SelectBoard` guided step.**
   Ordered after `InstallTools` (§6), with a CLI arm in the same PR — DOC-68
   §11's constraint, mechanically enforced by
   `cli_tests::every_command_variant_has_a_cli_invocation`
   (`crates/trusty-audit/src/cli.rs:261`).
7. **trusty-audit: the `state/` board block and the pinned identifier list.**
   Coordinates with #5494, which must encode "resume does not re-query" as a
   closure condition rather than citing §8.
8. **trusty-audit: pass the key to the `tga` child's environment**, with a test
   asserting it appears in no file anywhere under the working directory.
9. **trusty-review / tga: the board-derived report section**, including the
   detection-limits line and the no-board gap line (§9). Depends on #5405.
10. **A fixture-backed end-to-end test of the two-axis pre-sweep**, extending
    #5556's harness: recorded Linear responses behind a trait seam, no live
    workspace, consistent with DOC-68 §14 Q1.

---

*This document is the deliverable requested by
[#5641](https://github.com/bobmatnyc/trusty-tools/issues/5641). §12's six
questions are decided; §13's two need the owner. No `.rs` file was changed and
no issue in §14 has been filed.*
