# 0051. A project may declare that its dispatched agents work in the main checkout

- **Status:** Accepted
- **Date:** 2026-08-16
- **Scope:** crate `trusty-mpm` — `tm hook --pm-guard`
  (`pm_guard_worktree_grant::evaluate_worktree_grant`), the committed project
  config `core::project_config` (`DispatchIsolation`,
  `ProjectLevelConfig::dispatch_isolation`), and its resolver
  `project::worktree_policy::dispatch_isolation_for_project`
- **Reversibility Cost:** Low — the flag is opt-in and absent everywhere by
  default, so reverting it restores ADR-0048 decision 1 exactly and strands no
  data; the only residue is a `.trusty-mpm.toml` key that would then be rejected
  by `deny_unknown_fields` until removed
- **Decision Drivers:** ADR-0048 decision 1's unconditional grant; the owner's
  2026-08-16 report from `/Users/bob/Duetto/cto`, a markdown-only repo with no
  build, no tests and no concurrent writers, where every documentation agent
  wrote into a worktree the operator never saw and a second agent had to copy
  the files across; the finding that a prompt-level "do not isolate" instruction
  has no effect, because the grant is mechanical and never reads the prompt
- **Supersedes / Superseded by:** Amends
  [ADR-0048](0048-dispatched-writers-get-a-worktree-and-the-write-boundary-is-enforced.md)
  decision 1. ADR-0044's write boundary and ADR-0048's remaining decisions stay
  in force unchanged.

## Context

ADR-0048 decision 1 grants a worktree to every dispatched writer standing in a
main checkout, with no condition and no override. That was deliberate: the
reported harm was three sessions in one checkout with a commit landing on
another workstream's branch, and a false grant costs one worktree the harness
reclaims while a false allow corrupts a branch.

A project class exists for which the grant is pure cost. The owner's `cto` repo
is markdown only: no build, no test suite, one operator, no concurrent writers,
and a declared direct-to-main workflow with no branch, no PR and no review gate.
Five consequences were recorded there on 2026-08-16:

1. A `documentation` agent wrote a memo into its worktree; the files never
   appeared in the operator's checkout, and a second agent was dispatched to
   copy them across.
2. Every later revision repeated that pattern.
3. Agents dispatched with an explicit "do NOT use worktree isolation, edit in
   place" prompt instruction were isolated anyway, and worked around it by
   writing to the session scratchpad for the PM to copy in.
4. A `version-control` agent asked to commit on `main` reported BLOCKED: it was
   in a worktree, and its instructions told it to stop rather than proceed. The
   PM ran the git operations itself.
5. Several agent worktrees and branches were left behind.

Point 3 is the mechanism that makes this an architecture question rather than a
prompt question. `tm hook --pm-guard` decides the grant from the payload's
`tool_name`, `subagent_type`, `isolation` and `cwd`. It never reads the prompt,
so no instruction written into a dispatch can reach the decision. The only way
to exempt a project is to give the decision point something to read.

Two existing surfaces were candidates and both were rejected. The per-project
`worktree` flag from #3455 answers where the SESSION runs, and ADR-0044 decision
6 has already narrowed its live effect to the daemon-unreachable
framework-deployment fallback; overloading it would fuse two questions a project
may legitimately answer differently, and would silently change behaviour for the
projects that already set it. The machine-global registry (`projects.json`) is
per-host, cannot be reviewed in a PR, and — decisively — reaching it needs a
daemon that may be unreachable, which adds a failure branch to a decision that
must not have a permissive one.

## Decision

1. **A project may declare, in its committed `.trusty-mpm.toml`, that its
   dispatched agents work in the main checkout.** The key is
   `dispatch_isolation`, with two values: `"worktree"` (the default) keeps
   ADR-0048 decision 1's grant, and `"main-checkout"` suppresses it. Under the
   opt-out a dispatch made from a main checkout is left exactly as the PM issued
   it: cwd stays the checkout, no worktree is created, and nothing needs
   reclaiming.

2. **This reverses ADR-0048 decision 1 for the declaring project only.** The
   grant remains unconditional everywhere else, including in every project that
   ships no config, ships one that says nothing about this, or ships one that
   cannot be read. Isolation is still the default; this is an opt-out, and it
   never becomes an opt-in default by any path.

3. **The read fails CLOSED, and that direction is the decision, not an
   implementation detail.** Only an affirmative, successfully parsed
   `main-checkout` suppresses the grant. An absent file, an unreadable file, a
   file that is not valid TOML, a file carrying an unknown key, a file carrying
   an unrecognised token, and a file that simply sets no `dispatch_isolation`
   all resolve to `worktree`. The harm an accidental opt-out causes is silent —
   an agent writes into a shared checkout and no step reports an error — so no
   error, default, or falsy value may be what drops isolation.

4. **The direction is enforced by the type, not by the caller.**
   `DispatchIsolation` is an enum whose `Default` is `Worktree`, so every path
   that reaches for a default keeps isolation on. A `bool` would put the
   fail-closed direction in whichever caller last wrote `unwrap_or(…)`.

5. **The opt-out is a SEPARATE field from `worktree`.** `worktree` decides where
   the session runs (#3455, narrowed by ADR-0044 decision 6);
   `dispatch_isolation` decides where an agent that session dispatches runs. A
   project may want its session isolated and its agents beside it, or neither,
   and setting one must never move the other.

6. **The opt-out lifts the grant and nothing else.** ADR-0048 decision 3's
   concurrency deny still runs, so a second unisolated writer dispatched into a
   checkout the daemon reports as already held is still refused in an opted-out
   project. ADR-0044's write boundary and ADR-0049's staged-set commit gate are
   untouched: source writes in the checkout are still denied, and a
   documents-only commit is still permitted on the same terms as before. The
   opt-out is therefore useful precisely for the project class ADR-0049 already
   serves — one whose work is documents and configuration.

7. **What the operator accepts by setting it, stated so it can be quoted back.**
   Agents dispatched in that project write into the shared checkout. If two ever
   run concurrently and the daemon does not see one of them — a delegation
   record whose `SubagentStop` never arrived, a record stamped at a directory
   the query does not resolve, or a writer nothing dispatched at all — they
   share one git HEAD with no error at any step, which is the 2026-08-10
   incident ADR-0048 was written for. The bounds are: the project class that may
   set it is one with no concurrency pressure and no clean-build pressure,
   working through a declared alternate workflow (edit in place, commit to
   `main`, push; no branch, no PR, no review gate); and the setting is committed
   and reviewable, so it appears in a PR diff rather than in one operator's
   local state.

## Consequences

- **A `Task` dispatch in an opted-out project is no longer denied.** ADR-0048's
  `TASK_DENY_REASON` refuses a `Task` that needs a worktree it cannot be given.
  In a project that grants no worktrees, it needs none, so the check that
  produces that deny is skipped along with the grant. This removes a refusal
  rather than adding one.
- **The declaration is as trusted as the repository it is committed in.**
  Anyone who can push `.trusty-mpm.toml` can disable isolation for every agent
  dispatched in that checkout. That is the same trust level the file's existing
  `worktree` and `default_model` keys already carry, and the same level as
  `CLAUDE.md`, which shapes the PM's instructions outright. It is recorded here
  rather than left for a later reader to notice: this is not a new trust
  boundary, but it is a new thing on the existing one.
- **A misspelled opt-out silently does nothing except log.** `deny_unknown_fields`
  rejects the whole file, `load_or_report` logs at `error` level and yields
  nothing, and the operator gets worktrees. That is the correct direction and it
  is also the confusing one — an operator who writes `dispatch_isolation =
  "none"` sees no change and no message unless they read the hook's stderr. The
  alternative is failing open on a typo, which is the failure this ADR exists to
  prevent.
- **The grant now performs a second filesystem read per dispatch from a main
  checkout.** One `read_to_string` of a small file at the checkout root, on the
  dispatch path only, after every cheaper test has already passed. Ordinary tool
  calls never reach it.
- **The checkout ROOT is what gets read, not the cwd.**
  `main_checkout_root` already resolves a subdirectory to its checkout, and the
  grant now uses that resolution rather than the boolean `is_main_checkout` it
  used before, so a dispatch made from `docs/notes` reads the same declaration
  as one made from the root. Reading `<cwd>/.trusty-mpm.toml` instead would have
  made the opt-out apply or not depending on where the PM happened to be
  standing.
- **A worktree is still reachable in an opted-out project.** An explicit
  `isolation: "worktree"` on the dispatch is untouched — that branch returns
  before the config is read at all. The opt-out removes the automatic grant, not
  the operator's ability to ask.

## Alternatives Considered

- **Make the grant read the dispatch prompt.** Rejected. Point 3 of the evidence
  is that agents already ask for this in the prompt and are isolated anyway; the
  fix is not to start honouring prose. A prompt is written per dispatch by a
  model, so an instruction-driven grant would be decided by whatever the model
  happened to write, which is neither reviewable nor stable.
- **Overload the existing `worktree` flag.** Rejected — decision 5. It answers a
  different question, and every project that already sets `worktree = false`
  would have silently acquired the agent opt-out too.
- **Put the flag in the machine-global registry (`projects.json`), extending the
  `Project` record and the `tm projects` config form.** Rejected. It is
  per-host, so the declaration would have to be repeated by every operator and
  could never be reviewed; and reading it needs the daemon, whose
  unreachability would become a failure branch on a decision that must have no
  permissive one.
- **Spell the flag as `agent_worktree = false`.** Rejected — decision 4. A
  boolean puts the fail-closed direction in the caller, and the caller is the
  one place it must not live.
- **Suppress ADR-0048 decision 3's concurrency deny along with the grant.**
  Rejected — decision 6. The opt-out is a statement that the project has no
  concurrency pressure, not a request to stop checking. If two writers do land
  in the checkout, the deny is the only thing left that notices.

## Related Decisions

Vetted against the ADR corpus on 2026-08-16:

- **ADR-0048 (Dispatched writers get a worktree):** **Amends**, decision 1 only.
  The grant becomes conditional on a project's own declaration, with `worktree`
  as the default for every project that does not declare otherwise. Decisions
  2-10 are untouched: trusty-mpm still creates no worktrees (2); an unclassifiable
  agent is still treated as a writer where the grant applies (3); the write
  boundary is unchanged (4-6); the directory-keyed shared-writer query and its
  denies still run in an opted-out project (7, 10); `git fetch` is still
  unconditional (9).
- **ADR-0044 (Main-checkout write boundary):** **Consistent.** Decision 1's
  documents-and-configuration boundary is what makes the opt-out workable at all
  — an opted-out project's agents write documents in the checkout, which
  ADR-0044 already permits, and are still refused source writes there. Decision
  6 assigns the per-project `worktree` flag its one live effect, and decision 5
  of this ADR keeps `dispatch_isolation` clear of it: the two are separate
  fields answering separate questions, so decision 6 needs no amendment.
- **ADR-0049 (Documents-only commits in a main checkout):** **Consistent, and
  depended upon.** ADR-0049 is what lets an opted-out project's agent land its
  work: it edits `.md` in the checkout under ADR-0044 decision 1 and commits it
  under ADR-0049 decision 1. Without ADR-0049 this opt-out would produce writable
  files that could never be committed from where they were written — the exact
  incoherence ADR-0049 removed.
- **ADR-0037 (PM placement, main checkout by default):** **Consistent.** Nothing
  here changes where a session runs. This narrows what ADR-0037's default
  implies for the agents a main-checkout session dispatches, for one declared
  project class.
- **ADR-0036 (All worktrees under `.claude/worktrees/`):** **Consistent, no
  interaction.** An opted-out project creates no agent worktree, so the topology
  question does not arise; an explicitly requested one still lands at ADR-0036's
  location.
- **ADR-0018 (Loopback-only doctrine):** **Consistent, no interaction.** The
  declaration is read from a file on local disk, not over any socket, so this
  adds no network surface. That is also why it has no daemon-unreachable arm.

No Accepted or Proposed decision contradicts this amendment.
