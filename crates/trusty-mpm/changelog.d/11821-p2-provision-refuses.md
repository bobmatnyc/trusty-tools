Changed
- `launch_policy::provision_settings` and `provision_settings_default` return `Result<LaunchSettings, ProvisionError>`; the new `ProvisionError` carries the install-file error that refuses the launch (#11821).
