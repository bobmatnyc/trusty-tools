Changed

- `credentials` is now a top-level module, `trusty_common::credentials`, rather
  than a submodule of `inference` (closes
  [#4564](https://github.com/bobmatnyc/trusty-tools/issues/4564), epic
  [#4040](https://github.com/bobmatnyc/trusty-tools/issues/4040), DOC-45). The
  resolver was never inference-specific — four of its ten registry entries were
  Slack, Slack-user, Telegram and `claude-code` tokens, and its own doc comment
  already said *"Not limited to inference providers"* — so the path was actively
  misleading consumers into adding a raw `std::env::var` read instead of finding
  it. `trusty_common::inference::credentials` remains as a `#[deprecated]`
  re-export for one release; every in-tree consumer moved to the new path in the
  same change. The `credentials` cargo feature is unchanged and still builds
  without `inference-client`.
- The provider registry moved to `credentials::registry` and is now a
  `REGISTRY` table rather than a `match`, so it can be enumerated and asserted
  complete. `env_var_for` keeps its signature and its case-insensitive lookup;
  `registered_providers()` is new.
