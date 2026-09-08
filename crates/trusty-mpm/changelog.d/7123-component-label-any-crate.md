Fixed
- `tm issue audit` widens the accepted component-label set with the repository's
  own live `gh` labels (any label whose description reads `Crate: <name>`), not
  only the local `Cargo.toml` derivation #7123 shipped. A `gh` failure leaves
  the Cargo.toml-derived set unchanged rather than failing the audit. Refs #7123.
