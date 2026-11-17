Added
- PM sessions register `tm hook --completion-evidence` on `Stop` (#10575). It warns, never blocks, when the final message claims completion with no evidence map (verified / helper-reported / unfinished / untested), names an incomplete map, sets a size target with no `OWNER-REQUESTED-LIMIT: "<owner quote>"` line (`INVENTED_TARGET`), or carries that line without a quote.
