Fixed

- The technical-DD report's §2 Executive Summary no longer renders
  `_No data available — see Gaps & Caveats._` on a run without `--synthesize`
  (issue #5318). It was filled only from LLM synthesis prose, so every
  `tga audit` report collapsed the first section a diligence reader opens while
  listing real RED/AMBER findings in §5. §2 and its Top Risks table now roll up
  from the report's own data — applications, size, language mix, severity counts
  by dimension, and the application risk concentrates in — with the provenance of
  the figures used. Verified synthesis prose still wins when `--synthesize` runs.
  - When nothing measurable was supplied, §2 now names the specific missing
    inputs (no `metrics` file, no `--analyze` fetch, no scannable checkout)
    instead of collapsing to the generic Gaps & Caveats pointer.
