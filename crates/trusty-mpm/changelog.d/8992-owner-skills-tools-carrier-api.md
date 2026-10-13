Breaking
- `launch_policy::carrier::CarrierInputs` gains a public `profile: SessionProfile` field, and `provision_settings` / `provision_settings_default` take the launch's `SessionProfile` as a new last parameter (#8992). An Architect profile gets the owner `[mcp]` part only; pass `SessionProfile::Pm` to keep a PM launch's full policy. Code that builds a `CarrierInputs` with a struct literal must set the field.
- `launch_policy::carrier::settings_body` takes a `&SettingsParts` (MCP deny, connectors off, skills off, tools deny) instead of a deny set and a connectors flag (#8992).
