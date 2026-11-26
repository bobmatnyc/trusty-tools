Changed
- The boot config and boot lock live under `<config_dir>/.trusty-tools/trusty-installer/`, following the install paths. A broken `install.toml` fails `up` and the lock instead of using `~/.trusty-tools` (#11821).
