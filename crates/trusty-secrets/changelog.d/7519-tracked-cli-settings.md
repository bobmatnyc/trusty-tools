Security
- A tracked project config that sets a CLI `account` or `config_path`, at the top level or under `onepassword` or `keeper`, is refused on every build with `SecretsError::TrackedCliSettingRefused` (wire kind `tracked_cli_setting_refused`); set them in the machine config instead ([#7519](https://github.com/bobmatnyc/trusty-tools/issues/7519))
