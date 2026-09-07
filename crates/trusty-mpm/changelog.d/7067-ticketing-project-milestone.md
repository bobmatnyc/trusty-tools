Added

- The ticketing standard now requires a project and a milestone on every new
  issue. `agents.ticketing` takes three new keys — `default_project` (a GitHub
  Project title), `milestone_required` and `project_required`, both defaulting
  to `true` — and `tm issue standard` prints all three alongside a live section
  listing the repository's open milestones and the owner's open projects, read
  through the same `gh` runner the rest of `tm issue` uses. A `gh` failure
  prints `milestones: unavailable (<error>)` rather than an empty list, so a
  fetch failure cannot be read as "no milestone needed"; the requirement lines
  come from config and are unaffected. `tm issue seed-config` now also WRITES
  the `agents.ticketing` starter block carrying the new keys into
  `~/.trusty-tools/trusty-mpm/config.yaml` — the same file the runtime loader
  reads — since the lifecycle model and the standard live in different files.
  It creates that file if it is absent, appends the block textually if the file
  exists without one (every prior byte and comment preserved), and leaves an
  existing `agents.ticketing` exactly as the operator wrote it; it prints the
  path and which of the four outcomes happened. The fourth is a refusal: a file
  that already declares `agents:` without `ticketing:` is left untouched, since
  a second top-level `agents:` key would make it unparseable and discard every
  other setting in it — the block is printed for a manual paste under the
  existing key and the command exits nonzero, so a provisioning script cannot
  read a seed that did not happen as one that did. Writes go through the
  workspace's atomic config write (temp sibling, then rename), so an
  interrupted seed can never leave a half-written config. Every key in the
  written block is at its built-in default, so seeding does not change the
  standard. The
  `tm-ticketing` skill reverses its old "leave the milestone unset by default"
  rule: every new issue carries exactly one milestone chosen by a stated rule
  (the parent's, else the crate's `Backlog · <crate>`, else the one
  `tm issue standard` names), at least one project, and every relationship the
  brief names — an unset milestone is legitimate only with a
  `no-milestone: <reason>` comment on the issue (#7067).
