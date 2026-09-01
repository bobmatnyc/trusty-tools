Fixed

- The launchd-label drift scan no longer reads a plist's `CFBundleIdentifier` as
  a stray launchd label. launchd takes a job label from `<key>Label</key>` and
  from no other key, so `TrustyConsole.saver`'s bundle identifier turned
  `cargo test -p trusty-common` red on `main` and blocked every release, while
  the failure's own advice — derive it from the registry — would have
  invalidated the bundle's designated requirement (#6540, #5438, #2558). The
  codesign-identifier naming check also widened from `scripts/install-*-signed.sh`
  to every `scripts/*.sh` that runs `codesign`, which is what let the console-saver
  build script mint an unnamed identifier in the first place.
