Fixed
- The Slack gateway resolves `SLACK_APP_TOKEN` and `SLACK_BOT_TOKEN` from the process environment, then `.env.local`, then the trusty-common credential store, so it keeps starting after `tm doctor --fix-launchd-secrets` moves the tokens out of its LaunchAgent plist (#10120).
