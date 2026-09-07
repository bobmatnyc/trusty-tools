Added
- `tctl install` accepts two bundle keywords in place of member names (#4714):
  `core` installs trusty-memory + trusty-search (the always-on substrate `tctl
  up` boots as STAGE 1), and `agents` installs trusty-search, trusty-memory,
  trusty-review and trusty-mpm plus their runtime dependencies. Both mix freely
  with member names and expand transitively like any named member.
  - Each keyword derives its membership from an existing table — `core` from
    the boot manifest's `BootStage::Core`, `agents` from the stable set's
    `required` flag — so the keywords cannot drift from what they name.
  - An unknown member name now names the available keywords, so a mistyped
    `cores` is recoverable from the error alone.
