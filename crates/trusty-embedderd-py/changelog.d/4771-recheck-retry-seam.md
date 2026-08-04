Changed

- Internal only, no behaviour change: the `.ready` full-import recheck's
  retry policy and the venv-layout derivation are now separate, injectable
  seams (`recheck_with_one_retry`, `resolve_layout_in`). This lets their tests
  assert ordering and budgets directly instead of racing a sleeping subprocess
  against a wall-clock budget, and removes the process-global
  `TRUSTY_DATA_DIR_OVERRIDE` mutation from most of them entirely (#4125)
