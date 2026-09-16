Added
- Four tcode-native delivery-workflow agents in the embedded roster —
  `ticketing` (GitHub issues via `gh`), `version-control` (branch, commit,
  push, PR via `git`+`gh`), `local-ops` (build, test, version bump, changelog)
  and `documentation` — each with an explicit `tcode_tools:` allowlist that
  grants `bash`. `version-control` is new to the roster; the other three
  replace shared trusty-mpm bodies that projected to no allowlist at all.
- `pm.md` delegation guidance names the four and when to route to each, and
  tells the PM to parse the `ISSUE:` / `PR:` result lines they emit.
- Claude Code `tools:` frontmatter on a DISK agent is now translated into a
  trusty-code tool allowlist instead of being dropped, so an agent imported
  with `tcode paths import` carries the grant its author wrote rather than
  silently gaining every tool. `tcode_tools:` still wins when both are
  present, and the embedded roster is unaffected.
