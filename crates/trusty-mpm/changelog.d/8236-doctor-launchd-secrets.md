Security
- `tm doctor` gains a `launchd_secrets` row: it fails when a `com.trusty.*` LaunchAgent plist holds a plaintext credential, naming the file and the KEY and never the value, and reports UNKNOWN rather than OK when a plist cannot be read or parsed (#8236).
- `tm doctor --fix --yes` removes those entries in place, leaving every other key untouched. It takes no backup on purpose — a backup would be a second readable copy of the credential — and each step says the credential still has to be rotated. An unparseable or unwritable plist is reported as a failed step, never skipped.
