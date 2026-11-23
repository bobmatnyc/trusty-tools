Security
- Bundle-id scoping verified live against cua-driver 0.23.2: a `bundle_id` manifest entry admits a signed bundle but also any same-id impostor, so grants stay scoped to the exact pid and window; `tests/live_bundle_scope.rs` pins the finding with a signed fixture app (#11697).
