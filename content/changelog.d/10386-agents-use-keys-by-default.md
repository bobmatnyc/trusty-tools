Changed
- `tm-secrets` (1.5.0) and `secrets-manager` (0.3.0) describe the #10386 rule: an agent may use every in-scope key unless the operator denied it; `set`, `delete`, `copy` and removing a deny are operator-only, and `agent_use_refused` covers both cases.
