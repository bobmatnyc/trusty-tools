Added

- The search, memory, and analyze service dashboards each show a "Console"
  link-back in their Topbar: a same-origin relative link when served through
  the console's `/tools/<tool>/` mount, else the well-known standalone
  default `http://127.0.0.1:7788/` (owner ruling 2026-08-31, #6439). No
  config knob.
- The three dashboards' sidebar nav glyphs and status badges now draw from
  the shared Foundry icon set (`docs/design/UI/design-system/icons/
  ActionIcon.svelte`, vendored per dashboard) and the shared Foundry `Badge`
  component, instead of three divergent sets of ad hoc unicode characters
  and hand-rolled status spans (#6439).
