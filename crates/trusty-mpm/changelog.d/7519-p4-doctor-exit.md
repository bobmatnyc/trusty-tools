Changed
- `tm secrets doctor` exits non-zero when the server refused the project (not a checkout, no remote, or a remote off github.com); the `project:` line and the machine default's report are still printed ([#7519](https://github.com/bobmatnyc/trusty-tools/issues/7519))
- Under `CI=true` (or `CI=1`), `tm secrets doctor` exits non-zero when the selected backend is 1Password and no `OP_SERVICE_ACCOUNT_TOKEN` was present when the secrets server started ([#7519](https://github.com/bobmatnyc/trusty-tools/issues/7519))
