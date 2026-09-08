Fixed

- `scripts/install-console-saver.sh` restarts System Settings, `legacyScreenSaver`
  and `WallpaperAgent` after a successful install. All three cache the saver
  bundle's display-name metadata, so after #7129 renamed the bundle's
  `CFBundleName`/`CFBundleDisplayName` to "Trusty Console" the Screen Saver tile
  kept showing the old name across a reinstall until those processes were
  killed by hand. The script now does that itself and prints which processes it
  restarted ([#7128](https://github.com/bobmatnyc/trusty-tools/issues/7128)).
