Changed
- Each `indeterminate` row of `GET /registry/orphans` now carries `colocated` and `repo_identity`, the same registration metadata an `orphans` row already carried. A caller offering a per-row review of a root the daemon could not check needs to show what the registration is before an operator settles it.
