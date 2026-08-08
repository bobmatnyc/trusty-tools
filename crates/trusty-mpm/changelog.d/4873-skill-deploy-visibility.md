Fixed

- a skill the every-run deploy declines to refresh is now reported instead of skipped in silence: `ensure_managed_config_dir` emits one bounded warning naming the withheld files and pointing at `tm doctor --fix-skills --include-frozen` (closes [#4873](https://github.com/bobmatnyc/trusty-tools/issues/4873))
  - the skip itself is unchanged and still correct — a hand-edited (checksum-frozen) skill and an unmanaged project-custom skill are both preserved
  - corrects the `resume_managed` and `run_inplace_relaunch` comments that claimed agent/skill redeploy and MCP injection were not re-run on those paths; all three run paths reach `ensure_managed_config_dir` through `prepare_managed_config`, so they always were
