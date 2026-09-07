<!-- placeholder: replaced by PR feat/statusline-savings-ledger -->

# Statusline cost savings

## Cost savings

The `💸` segment on the `tm` statusline is an estimate of the tokens the harness
kept out of the session — the bulk reads it diverted to a subagent or a filtered
file read instead of the main context, plus the instruction text it compressed
before the model ever saw it. It is an estimate rather than a measurement: the
counterfactual session that read everything in full never ran, so the figure is
what the diversion and compression avoided, priced at the model's own rate.

The segment sits at the end of the statusline, after the context gauge, and
reads `0` until the first diversion of the session lands.
