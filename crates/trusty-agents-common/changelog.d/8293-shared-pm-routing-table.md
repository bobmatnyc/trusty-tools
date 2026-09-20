Added

- `pm_routing`: the one agent-routing table trusty-mpm's PM instructions and
  trusty-code's delegate-mode PM card are both rendered from. One row per class
  of work carries a per-consumer route; `fill` substitutes the rendered table
  and pipeline chain into each product's authored template, returning
  `FillError::DuplicatePlaceholder` rather than rendering a template that
  authored either marker twice. The public surface is `fill`, `FillError`,
  `Consumer`, `agents`, `render_table`, `render_pipeline`, the two placeholder
  constants and `MPM_PIPELINE` / `TCODE_PIPELINE`; the rows themselves stay
  crate-private (#8293).
