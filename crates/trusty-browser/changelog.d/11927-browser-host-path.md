Fixed

- `install-host`, `install-service` and `launchd-plist` never record a cargo build-directory binary: they register the `cargo install` binary instead, or refuse with the rejected path and the install command ([#11927](https://github.com/bobmatnyc/trusty-tools/issues/11927))
- The native-host launchers answer a missing binary with an error naming its path and the repair step, so the popup's Retry shows it instead of "Native host has exited" ([#11927](https://github.com/bobmatnyc/trusty-tools/issues/11927))
- `install-service` and `uninstall-service` take over a launcher or plist whose binary is gone (not found; an unreadable binary still counts as present), and the launcher form 0.8.2 wrote, instead of refusing them as unrelated ([#11927](https://github.com/bobmatnyc/trusty-tools/issues/11927))
- `uninstall-service` run from a build binary also matches the registration of the installed binary it would substitute, and its own ([#11927](https://github.com/bobmatnyc/trusty-tools/issues/11927))
- A binary in any cargo profile directory (such as `target/ci-smoke/`) counts as a build path; a `debug` or `release` segment deeper under a `target` directory no longer does ([#11927](https://github.com/bobmatnyc/trusty-tools/issues/11927))
