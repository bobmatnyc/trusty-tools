Added
- `tm secrets doctor` prints each unavailable backend's reason and fix, the account machine config, the selected backend's posture, and whether a 1Password service-account token was present at server start; an available 1Password row with no token says it needs an unlocked app or an `op signin` session ([#7519](https://github.com/bobmatnyc/trusty-tools/issues/7519))
- `tm secrets doctor` lists each installed unsupported secrets tool (DOC-74 §7) with its path, and the rest on one line ([#7519](https://github.com/bobmatnyc/trusty-tools/issues/7519))
