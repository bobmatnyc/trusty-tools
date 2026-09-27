Fixed

- **The four supporting agents a delegate-mode PM routes to are now pinned as
  resolvable with the tool grant their own card declares (#8227).** `research`,
  `documentation`, `ticketing` and `version-control` each resolve through the
  same `resolve_agent` call `delegate_to_agent` makes, arrive with a non-empty
  prompt and the `role:` their card declares, and project exactly the
  `tcode_tools:` allowlist the card wrote — compared against the card's own
  frontmatter rather than a transcribed list, so an edit to either side fails.
  #8129 had verified roster presence only, which left the #8199 class of defect
  (a frontmatter key silently dropped in projection, leaving an agent with a
  grant nobody chose) undetected for exactly the four agents the PM's routing
  table now depends on.
