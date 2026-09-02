Added
- `trusty-analyze doctor` warns when a retired LaunchAgent plist is still on
  disk (`~/Library/LaunchAgents/com.trusty.analyze.plist` from a pre-#6350
  install), naming `trusty-analyze service uninstall` as the way to clear it.
  The check only reports; it never deletes (#6621).
