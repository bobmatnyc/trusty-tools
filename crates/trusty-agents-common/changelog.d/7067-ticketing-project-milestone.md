Changed

- The bundled `ticketing` agent no longer leaves the milestone unset by default.
  Its "Milestones Are Release Slots" section becomes "Milestone, Project,
  Relationships — Set on Every New Issue": every issue it files carries exactly
  one milestone (the parent's, else the crate's `Backlog · <crate>`, else the
  one `tm issue standard` names), at least one GitHub Project, and every
  relationship the brief names, all set natively on the `gh issue create` call
  — `--milestone`, `--add-project`, `--parent`, and the `dependencies/blocked_by`
  API for blocked-by. Titles come from `tm issue standard`, never hand-typed. An
  issue may carry no milestone only with a `no-milestone: <reason>` comment on
  it, and the agent verifies its own filing with
  `gh issue view N --json milestone,projectItems` before reporting it done
  (#7067).
