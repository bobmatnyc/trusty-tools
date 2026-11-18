Fixed
- The crate builds clean on Linux: the Darwin-only peer imports are gated with the code that uses them, and the launchd label comes from the shared `trusty_common::launchd_labels::BROWSER` registry entry (same `com.trusty.browser` value)
