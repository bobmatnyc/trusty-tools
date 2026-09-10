Added

- `tctl` now reports `rtk` as a trusty-mpm prerequisite. Detection only: the hint says `install with `brew install rtk`; do not run `rtk init`, tm invokes rtk directly`, and the row carries no auto-install command on any platform, since `rtk init` / `rtk init -g` install a PreToolUse Bash hook that competes with tm's own (#7311).
