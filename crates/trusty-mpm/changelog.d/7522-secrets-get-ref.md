Added
- New `secrets_get_ref` MCP tool resolves a key name or `secret://` reference to its reference for one project, through the trusty-secrets socket, on demand. It answers `reference`, `key`, `present`, `scope`, `vault`, `backend` and `imported_at`, and never a value, a masked head or a length. Nothing calls the socket at session start (#7522).
