Added

- Bundled a `tm-secrets` framework skill — the operator/PM reference for `tm secrets` (1Password / Keeper / macOS Keychain behind `trusty-common`): the store model and default backend, the full command grammar (marked pending against the CLI child issues that ship it), `tm secrets exec` as the only sanctioned way to hand a resolved value to a subprocess, the copy-between-stores and import flows, and the never-print-a-value rules (refs [#7527](https://github.com/bobmatnyc/trusty-tools/issues/7527), epic [#7517](https://github.com/bobmatnyc/trusty-tools/issues/7517))
