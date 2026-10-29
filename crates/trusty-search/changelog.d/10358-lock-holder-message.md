Fixed
- `trusty-search start` no longer calls a healthy lock holder "the OLD image" or advises `kill -TERM` against it. It says another instance is already serving and exits; the old text stays only when the binary on disk is newer than the holder. The launchd respawn loop is a separate fix (#10358).
