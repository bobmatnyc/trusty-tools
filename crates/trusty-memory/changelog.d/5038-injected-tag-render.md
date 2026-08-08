Fixed

- **The injected drawer preview no longer renders storage provenance or an
  uncapped tag list (closes [#5038](https://github.com/bobmatnyc/trusty-tools/issues/5038)).**
  `compose_injection` walked every tag on every recalled drawer, so
  `creator:client`, `creator:version`, `creator:source` and `creator:cwd` — the
  last a ~90-character absolute path — rendered on each of ~12 drawers per
  firing, alongside a median 17-tag topical list. Measured over 17,176 real
  firings in the enriched-prompt log, the `_(tags: …)_` suffix was **53.0% of
  every byte the hook injected**, outweighing the content preview it labels on
  82.3% of drawers. `INJECTION_BYTE_CAP` never fixed that — it only meant the
  noise evicted real drawers instead of growing the block. Rendering now drops
  the `creator:*` namespace (via `attribution::is_creator_tag`, the predicate the
  TUI and dashboard already hide those tags with) and caps the rest at 4 with a
  `+N more` marker, returning 40.1% of the injection to content. The tags are
  untouched in storage and still returned by `memory_list` / `memory_recall`.
- The rendered tag suffix is now bounded in characters as well as count. The
  4-tag cap was argued from "a label must never outweigh the payload it labels"
  (220 characters) but enforced only a count, and four long hyphenated tags —
  `slate-prioritization-in-flight` and friends, both real tags from the live
  palace — exceed that budget while satisfying the cap.
