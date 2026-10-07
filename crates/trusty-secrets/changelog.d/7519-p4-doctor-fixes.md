Fixed
- `secrets.doctor` lists `onepassword` and `keeper` on every build, as `not_compiled` without `cli-backends`, instead of dropping their rows ([#7519](https://github.com/bobmatnyc/trusty-tools/issues/7519))
- Off macOS the server's factory refuses `keychain`, so doctor no longer shows a healthy Keychain row on a Linux host that has none ([#7519](https://github.com/bobmatnyc/trusty-tools/issues/7519))
- An unreadable or unparsable account machine config is reported as `config_invalid`, not as a backend that is merely off ([#7519](https://github.com/bobmatnyc/trusty-tools/issues/7519))
- A project whose tracked config is refused gets a doctor report with the refusal on its selected row, instead of a failed call ([#7519](https://github.com/bobmatnyc/trusty-tools/issues/7519))
