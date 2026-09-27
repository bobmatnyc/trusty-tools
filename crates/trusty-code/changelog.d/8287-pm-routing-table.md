Added

- **The delegate-mode PM now has a routing table naming `research`, `engineer`
  and `qa-agent` as delegation targets, so a simple coding task is routed
  without the user naming an agent (#8287).** `pm.md` previously documented only
  the four delivery-workflow specialists (`ticketing`, `version-control`,
  `local-ops`, `documentation`), so a `tcode tui --delegate` run had no basis to
  dispatch context-gathering or verification at all. The card now carries one
  contiguous, marked routing block covering all seven targets, with the coding
  path fixed at `research` then `engineer` then `qa-agent`. Verification goes to
  `qa-agent` and explicitly not to `qa`: DOC-75 §6 requires real test output in
  the transcript and tcode's `qa` fork carries no `bash`, so it can recommend
  commands it cannot run. The block is delimited by
  `crate::assets::PM_ROUTING_BLOCK_BEGIN`/`_END` and readable via
  `crate::assets::pm_routing_block`, which is the span #8293 lifts to a shared
  source. Three tests hold the line: every agent the block names must resolve
  and be delegable, the three coding-path targets must appear in order in the
  ASSEMBLED delegate-mode prompt, and the card must name no tool a delegating PM
  registry lacks while staying under DOC-75 §4b's 2x size cap (3673 of 5726
  bytes).
