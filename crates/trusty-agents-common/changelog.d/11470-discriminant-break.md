Breaking
- `AgentContentError::BelowMinimum` sits before `Write`, so the implicit discriminants of `Write` and later variants change (`enum_no_repr_variant_discriminant_changed`); bumped to 0.10.0 (#11470).
