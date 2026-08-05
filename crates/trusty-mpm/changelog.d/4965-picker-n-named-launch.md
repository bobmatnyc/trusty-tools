Added

- the interactive session picker (bare `tm`, `tm ls`) accepts `n` to launch a new session and `n <name>` to name it (closes [#4965](https://github.com/bobmatnyc/trusty-tools/issues/4965))
  - `n` is slot-independent: the `[N] launch new session` number shifts as sessions come and go, so there was nothing to memorize — that entry stays, and `n` is additive
  - `n <name>` forwards the raw string as `name_hint`, which the daemon sanitizes the same way `tm session new --name-hint` already does, so `n my auth fix` yields `tm-my-auth-fix-01`
