Fixed
- The Architect `items` schema spells the `status_briefs` brief pattern's whitespace class out as explicit characters, so the Rust schema loader accepts it; it accepts and refuses the same strings as before (#10697).
