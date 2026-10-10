Added
- `GrantRegistry::check_scoped`, which runs `authorize_scoped`'s checks without spending a one-shot grant, and `GrantRegistry::spend`, which spends one use after the caller's own checks passed (#9621).
