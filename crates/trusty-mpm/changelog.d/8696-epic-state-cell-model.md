Fixed

- The epic tracker's `phases` block State cell now resolves a phase's labels
  through the issue state model — the state whose `label.name` the phase
  carries, rendered as the state name minus `status_prefix` — instead of
  matching raw labels by prefix. `tm issue transition` on a phase whose model
  labels a state without the prefix (e.g. `status:in-progress` labelled
  `in-progress`) no longer leaves the row at `open` and reports the block
  "already current"; `tm issue epic sync`, `create` and the `tm issue audit`
  phases-block row render through the same model, so a stale block is now
  regenerated and FAILed rather than passed. A prefixed label no model state
  issues reads `open`, the answer `tm issue current` gives
  (refs [#8696](https://github.com/bobmatnyc/trusty-tools/issues/8696))
