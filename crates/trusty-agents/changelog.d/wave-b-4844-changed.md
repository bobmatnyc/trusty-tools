Changed

- The bundled flat `cto-assistant.toml` no longer declares the
  `gworkspace-calendar` skill
  ([#4844](https://github.com/bobmatnyc/trusty-tools/issues/4844)). No such
  skill file exists on disk, so the declaration never resolved.
