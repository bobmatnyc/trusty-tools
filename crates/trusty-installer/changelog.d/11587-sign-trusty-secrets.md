Added

- `tctl sign trusty-secrets` signs the registry-installed `trusty-secrets` binary with the Developer ID identity (identifier `com.trusty.trusty-secrets`), so its Keychain items keep trusting it across upgrades. `scripts/install-trusty-secrets-signed.sh --version <v>` runs `cargo install trusty-secrets --version <v> --locked` and then the sign step, and exits non-zero if either fails (Refs #11587).
