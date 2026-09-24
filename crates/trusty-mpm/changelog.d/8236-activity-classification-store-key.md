Fixed
- The managed-session activity route surfaces `classification` when `OPENROUTER_API_KEY` lives in the credential store rather than the daemon's environment (#8236). It read only the process environment, so moving the key out of the LaunchAgent plist hid the field after a restart.
