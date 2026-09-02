Added

- `launchd_claim` — `socket_owner` decides whether a launchd unit owns the socket
  an on-demand spawn is about to bind, and `launchd_socket_owner` reads the two
  registration signals off the real launchd. `launchctl print` alone cannot
  answer it: in the bootout/bootstrap window that loses the race the unit is
  booted out and launchd reports nothing, so the INSTALLED PLIST counts as
  registration too (#6619). Not macOS-gated, so cross-platform daemon guards call
  it without a `cfg` split; off macOS it answers "no unit".
- `launchd::plist_path_for_label` and `launchd::label_is_loaded` — the label-only
  readers behind that check. `LaunchdConfig::plist_path` and
  `LaunchdConfig::is_loaded` now delegate to them, so there is one spelling of
  `~/Library/LaunchAgents/<label>.plist` and one `launchctl print`.
