# tm-ticketing — default standard and behaviour

## Default standard — the taxonomy

Each row states the default a repository gets when its `TICKETING.md` is silent,
and where this skill specifies it in full.

| Axis | Default | Detail |
|---|---|---|
| Type label | exactly one of `bug`, `enhancement`, `refactor`, `chore`, `documentation`, `epic` | "Labels" |
| Component label | one or more, read off the file paths the finding cites, naming the project's own stack unit; none when no unit owns the path, plus a `no-component-label: <reason>` comment | "Labels" |
| Priority label | `P0`–`P3`, applied **only** when the issue text itself asserts severity | "Labels" |
| Workstream label | `ws/<session>`, the filing session's | "The Labels, Project, Milestone Standard" |
| Milestone | parent's milestone → owning crate's `Backlog · <crate>` → the one `tm issue standard` names; else a `no-milestone: <reason>` comment | "Choosing the Milestone and the Project" |
| Project | by crate/topic fit from the live list, or `default_project`; attached with `gh project item-add <number> --owner <owner>` | "Projects" |
| Relationships | native sub-issue and blocked-by only; `Refs #N` is fix linkage, never an issue-to-issue link | "Relationships (native, never prose)" |
| Lifecycle states | `open → status:in-progress → status:coded → status:merged → status:tested → closed`, mutually exclusive, moved in step with the project's `Status` field | "Lifecycle", "Every issue: milestone, relationships, state" |
| Dedupe disposition | `COMMENT` / `REOPEN` / `NEW REGRESSION` / `NO TICKET`, one per finding, reported by name | "Search first, then choose a disposition" |
| Epic title | `[EPIC <epic#>] <the outcome, in plain words>`, created as `[EPIC]` and renamed once the number is known | "Trackers and phase issues" |
| Phase title | `[EPIC_<epic#> PHASE_<n>] <what this phase does>`, a native sub-issue of the tracker | "Trackers and phase issues" |
| Research | a committed doc under `research_docs_path`, linked from the tracker — never pasted into an issue | "Trackers and phase issues" |
| Title | `<type>: <what is wrong or wanted>`, under ~70 characters | "What a Ticket Says" |
| Body | problem, decisive evidence, 1–4 observable closure conditions; no structured headings | "What a Ticket Says" |
| Attribution | one `🤖🤖🤖 Generated with trusty-mpm — …` line ending every issue body and comment | "Attribution on Issues and Comments" |

## Default behaviour

Every setting below is mutable by `TICKETING.md`, and the generated file states
each one as an explicit value so a user edits it rather than discovering it.

| Setting | Default |
|---|---|
| `comment_on` | Claim at dispatch, `status:coded`, `status:merged`, `status:tested`, close, blocked. A review verdict posts a comment only when it changes the issue's state |
| Claim comment carries | the claiming session name and the date |
| `status:coded` comment carries | the PR link |
| `status:merged` comment carries | the squash SHA |
| `status:tested` comment carries | what was run against the installed artifact and what it printed |
| Close comment carries | the same evidence, as `tm issue transition N closed --note` |
| Blocked comment carries | the blocker, its impact, and the unblock criteria |
| `transitions` | Event-driven and automatic: the agent that observed the event owes the label pass in the same dispatch. Nothing sweeps for stale labels later |
| `assign` | `--assignee @me` on creation |
| `set_milestone` / `set_project` | yes, both on the creating call, never in a follow-up pass |
| `create_missing_labels` | yes, after `gh label list` confirms absence; `tm issue seed-labels` first for the harness's own set |
| `dedupe_strictness` | search open AND closed on every key the project names, before every filing. `COMMENT` beats `NEW` whenever an open issue covers the same defect |
| `comment_verbosity` | pointer-first: links and the decisive line, evidence inline only where the reader cannot act without it. Never a per-poll progress log |
| `rollup_issue` | none by default — a project names one for sub-HIGH review and self-improvement findings, which then never become their own issue |
| `pr_issue_link` | `Refs #N` (fixed — `SKILL.md`, "The Standard of Record — `TICKETING.md`") |
| `status_on_creation` | plain `open`. Filing is not a dispatch; `status:in-progress` waits for a brief that says work starts now (#7803) |
| `epics.title_format` | `[EPIC <epic#>] <outcome>` for the tracker (created `[EPIC]`, renamed once the number is known); `[EPIC_<epic#> PHASE_<n>] <what>` for a phase issue |
| `epics.tracker_autoupdate` | `true` — the agent regenerates the `<!-- phases:start -->` block wholesale from child state, amends `<!-- deferred:start -->`, and changes nothing outside the markers |
| `epics.update_triggers` | exactly four: a phase opens, a phase closes, a phase blocks or unblocks, an item is deferred or a deferred item lands |
| `research_docs_path` | `docs/research/<effort>/` — research lives there, committed, and the tracker links to it; no issue body carries findings |
| `followups.budget_per_phase` | `2` standalone follow-up issues per phase issue |
| `followups.severity_floor_for_standalone` | `HIGH` — below it, or past the budget, the follow-up is a checklist line, not an issue |
| `followups.tracker` | the epic's Follow-ups checklist when the work has an epic; otherwise the project's `rollup_issue` |
| `followups.due_within_days` | `14` — a standalone follow-up carries a milestone or a `due-by` date inside that window |
| `staleness.stale_after_days` | `30` with no activity and no milestone → label `stale` and post one triage comment |
| `staleness.close_stale_after_days` | `60` → close with a note |
| `staleness.exempt_labels` | `[keep]` |
| `staleness.exempt_when_milestoned` | `true` |
| `staleness.decision_request` | digest — recommendations grouped per epic in ONE comment on the epic tracker, never a question per issue |
| `coverage` | every open issue belongs to an epic or to a `Backlog · <area>` milestone; neither is a standard violation the audit reports |
