Fixed

- `tm hook --pm-guard` lets a read-only agent (`security`, `research`,
  `code-critic`, `code-analyzer`, `Explore`, `Plan`) run
  `GH_CONFIG_DIR=<dir> git ls-remote ...`, so the pre-push base-ref check
  (#7748) can use the project's gh credential. `<dir>` must be an absolute
  literal path, and the prefix is admitted only
  before `git ls-remote`; every other environment prefix, and this one before
  any other command, stays refused
  ([#8628](https://github.com/bobmatnyc/trusty-tools/issues/8628)).
