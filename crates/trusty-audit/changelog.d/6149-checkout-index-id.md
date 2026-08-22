Fixed

- The grounding pass indexes each audited checkout under an id derived from its
  canonical path (`trusty_common::derive_checkout_index_id`) instead of the
  checkout basename, so a machine already serving an index of the same name for
  a different tree no longer collides. The confirmation run of 2026-08-21 hit
  exactly that: the engagement clone at `…/repos/local/trusty-tools` derived
  `trusty-tools`, the daemon was serving `~/Projects/trusty-tools` under it, and
  the root-mismatch guard correctly refused — which cost the report both its
  evidence discovery and its complexity hotspots. That guard stays, as the
  backstop for an index registered under the old scheme or a tree that has since
  moved. Existing indexes registered under a bare basename are not renamed;
  the first audit of a checkout re-indexes it under the new id.
- The investigation budget reaches `[report].investigate_max_files` /
  `investigate_max_bytes` even when both grounding legs degrade. The budget is
  configuration, not evidence, and it used to be written only alongside a
  non-empty ranking — so a run that lost its index also lost its 240-file budget
  and trusty-review fell back to its own 40-file default, compounding one
  failure into two.
