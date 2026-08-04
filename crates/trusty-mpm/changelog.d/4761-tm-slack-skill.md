Added

- New bundled `tm-slack` skill folding `tm-slack-canvas-delivery` into general Slack delivery guidance — routes through the native `mcp__slack-mcp__*` connector per ADR-0014, enumerates the verified 22-tool surface, and generalizes the "creation is not delivery" completion rule to every delivery shape (message, canvas, scheduled) (closes [#4761](https://github.com/bobmatnyc/trusty-tools/issues/4761))
  - registration into `bundle_all.rs`'s `ALL` table is a separate, sequenced follow-up (that file was held by a concurrent PR); the skill does not ship until that line lands
