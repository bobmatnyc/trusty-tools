Fixed

- `trusty-search service --help` named `com.trusty.trusty-search.plist`, a launchd unit that does not exist; it now names the live `com.trusty.search.plist`, and a test fails if the help text drifts from the launchd label registry (part of #8253; the `trusty-console` help text is tracked separately)
