Added
- `GET /registry/orphans` lists registrations in `indexes.toml` whose root is
  gone, separately from the ones the daemon declined to judge. It reads the
  registry file rather than the in-memory registry, so it can see a registration
  the warm-boot allowlist excluded — which `GET /indexes` cannot (#6371, #6363).
  It removes nothing; `DELETE /indexes/{id}` stays the one deregistration path.
