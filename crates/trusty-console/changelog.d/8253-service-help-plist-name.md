Fixed

- `trusty-console service --help` named `com.trusty.trusty-console.plist`, a launchd unit that does not exist; it now names the live `com.trusty.console.plist`, and a test fails if the help text drifts from the launchd label registry (closes the console half of #8253)
