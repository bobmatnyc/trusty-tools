Added
- Channels and listeners are one concept: a `Channel` type carries the union of a harness listener, a per-assistant listener binding and a channel binding, with `route_to` for global fan-out and a stated precedence rule (an assistant's own binding wins over a global one on the same provider and destination).
- `~/.trusty-agents/config.toml` `[[listeners]]` migrates to `[[channels]]` once, on load. The legacy table is left in place and keeps working; a table that does not parse is reported by name and migrates nothing.
- An assistant's `agent.toml` `[[listeners]]` bindings read as channels, and migrate once into its channels file when they name a destination the provider accepts.
