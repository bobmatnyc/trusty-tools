Changed
- `tm-secrets` (1.5.0) and `secrets-manager` (0.3.0) describe the #10386 rule: an agent may use every in-scope key unless the operator denied it; `set`, `delete`, removing a deny and a copy to a sync target are operator-only, and `agent_use_refused` covers both cases.
