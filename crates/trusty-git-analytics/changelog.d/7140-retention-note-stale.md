Fixed

- `tga audit`'s Gaps & Caveats data-handling line now runs the real
  data-retention attestation #5218 shipped, instead of unconditionally
  quoting the pre-#5218 placeholder saying an attestation is "pending
  (#5218)" (#7140). Every DD report generated after `tga` 7.1.0 carried that
  stale sentence even though #5218 had already shipped. The line now states a
  clean scan's table/column coverage, names finding counts when the scan
  turns up a content-bearing column or a diff-shaped row, and — only if the
  scan itself cannot run — says so and why, still stating tga's schema-level
  no-file-content claim rather than falling silent.
