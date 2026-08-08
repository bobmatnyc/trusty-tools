Changed

- The daemon and supervisor launchd labels used by the autostart gate, the
  launchd probe, the unsupervised-daemon refusal, and the MCP bridge's no-spawn
  hint now come from `trusty_common::launchd_labels` instead of four separate
  literals. Values are unchanged. The hints matter most: a remedy string naming
  a plist the host does not have is exactly #2827, and #1900 was itself a
  label-lookup miss (#4868)
