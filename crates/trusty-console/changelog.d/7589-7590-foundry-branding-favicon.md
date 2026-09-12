Added

- The search, memory and analyze dashboards open their Topbar with the Foundry
  brand lockup — the canonical robot mark beside the tool's own name — and each
  ships a branded `<title>` and the shared trusty favicon. A dashboard served at
  `/tools/search/` previously showed a bare breadcrumb, the generic page icon,
  and nothing identifying the product family
  ([#7589](https://github.com/bobmatnyc/trusty-tools/issues/7589)). The lockup is
  a new canonical design-system component, `ToolLockup.svelte`
  (`docs/design/UI/design-system/icons/`), vendored into each dashboard with
  `RobotIcon.svelte`; it reads colour and type from Foundry tokens only, so it
  inverts with `data-theme` and introduces no new hue.
- One canonical trusty favicon,
  `docs/design/UI/design-system/icons/favicon.svg`, derived from the Foundry
  robot mark and carried byte-for-byte by every trusty-\* web page: this crate's
  console UI and three dashboards, the public website, and the trusty-audit,
  trusty-code-gui and trusty-mpm-gui shells. Favicons were per-crate and
  divergent, and four of those pages shipped none at all
  ([#7590](https://github.com/bobmatnyc/trusty-tools/issues/7590)). The console's
  own `docs/design/UI/icons/trusty-console-favicon.svg` is deleted with it;
  `trusty-agents` keeps its separate product favicon under the standing owner
  exemption.
