Added

- The ticketing standard now requires a project and a milestone on every new
  issue. `agents.ticketing` takes three new keys — `default_project` (a GitHub
  Project title), `milestone_required` and `project_required`, both defaulting
  to `true` — and `tm issue standard` prints all three alongside a live section
  listing the repository's open milestones and the owner's open projects, read
  through the same `gh` runner the rest of `tm issue` uses. A `gh` failure
  prints `milestones: unavailable (<error>)` rather than an empty list, so a
  fetch failure cannot be read as "no milestone needed"; the requirement lines
  come from config and are unaffected. `tm issue seed-config` now also prints a
  copy-pasteable `agents.ticketing` starter block carrying the new keys, since
  the lifecycle model and the standard live in different files. The
  `tm-ticketing` skill reverses its old "leave the milestone unset by default"
  rule: every new issue carries exactly one milestone chosen by a stated rule
  (the parent's, else the crate's `Backlog · <crate>`, else the one
  `tm issue standard` names), at least one project, and every relationship the
  brief names — an unset milestone is legitimate only with a
  `no-milestone: <reason>` comment on the issue (#7067).
