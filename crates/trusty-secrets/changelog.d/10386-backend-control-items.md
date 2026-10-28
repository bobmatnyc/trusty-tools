Breaking
- `SecretBackend` replaces `agents_may_use`/`set_agents_may_use` with `holds_agent_controls`, `control_item` and `set_control_item` over `store::ControlItem`. A backend that stores no control item keeps the defaults, which refuse with `unsupported` (#10386).
