Added
- `RpcRouter::mark_liveness` / `typed_liveness` classify a method as a liveness
  check. `serve_until_idle` answers such a method normally but does not restart
  the idle window for it, so a monitor polling faster than the window can no
  longer keep an on-demand service resident. A connect-and-close probe was
  already exempt; a health RPC was not (#6621).
- `launchd::user_plist_path(label)` resolves `~/Library/LaunchAgents/<label>.plist`
  from a label alone. `LaunchdConfig::plist_path` delegates to it (#6621).
