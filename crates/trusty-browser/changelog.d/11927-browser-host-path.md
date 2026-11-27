Fixed

- `install-host`, `install-service` and `launchd-plist` never record a cargo build-directory binary: they register the `cargo install` binary instead, or refuse with the rejected path and the install command ([#11927](https://github.com/bobmatnyc/trusty-tools/issues/11927))
- The native-host launchers answer a missing binary with an error naming its path and the repair step, so the popup's Retry shows it instead of "Native host has exited" ([#11927](https://github.com/bobmatnyc/trusty-tools/issues/11927))
- `install-service` replaces a launcher or plist whose binary is gone, and the launcher form 0.8.2 wrote, instead of refusing them as unrelated ([#11927](https://github.com/bobmatnyc/trusty-tools/issues/11927))
