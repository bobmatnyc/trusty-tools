Documentation

- Corrected `#4868` issue citations in `launchd_labels`, `launchd`,
  `launchd_activate`, and this crate's module index to `#4919` — the actual
  origin of the launchd-label registry work
  ([#5449](https://github.com/bobmatnyc/trusty-tools/issues/5449)). `#4868` is
  an unrelated trusty-search shutdown-flush-budget fix; three genuine
  backward-references to that real fix (its `ExitTimeOut` plist key) are
  unchanged.
