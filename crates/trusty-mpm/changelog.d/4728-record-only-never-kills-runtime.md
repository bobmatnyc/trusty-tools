Security

- auto-prune can no longer destroy anything outside the session registry: `decommission_record_only` is now a dedicated single-effect function rather than a flag on the destructive teardown (closes [#4728](https://github.com/bobmatnyc/trusty-tools/issues/4728))
  - as a flag it left two destructive effects ungated — a runtime SIGTERM plus `kill_session` against any live pane sharing the record's name, and a cross-daemon `DELETE /indexes/{id}` whose target resolved to the PARENT PROJECT's search index once the workspace was gone
  - both were reachable from `tm ls` in 1.3.3 and 1.3.4; neither required a name collision
  - `decommission_with_root` keeps every effect, no longer takes a `record_only` parameter, and now documents its side effects as an explicit table
