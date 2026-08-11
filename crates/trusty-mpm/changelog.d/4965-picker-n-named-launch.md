Added

- the interactive session picker (bare `tm`, `tm ls`) accepts `n` to launch a new session and `n <name>` to name it (closes [#4965](https://github.com/bobmatnyc/trusty-tools/issues/4965))
  - `n` is slot-independent: the `[N] launch new session` number shifts as sessions come and go, so there was nothing to memorize — that entry stays, and `n` is additive
  - the token must be exactly `n`/`N`, and a name requires a whitespace separator, so `new`, `no`, `nn`, `n1` and `nauth` are all unrecognised — no n-initial typo can spawn a session
  - `n <name>` launches as `tm-<name>-NN`, with the name kebab-cased in the CLI before it is sent (`n My Auth Fix!` → `tm-my-auth-fix-01`); sanitizing here is what keeps `n feature/auth` and `n hotfix/auth` distinct, since the daemon's own hint sanitization takes the path basename and would collapse both to `auth`
  - a name with no usable characters (`n !!!`) is refused with a message rather than spawning under the daemon's shared `local` fallback leaf
