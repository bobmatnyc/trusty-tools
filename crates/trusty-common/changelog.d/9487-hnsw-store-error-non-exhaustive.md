Breaking
- `HnswStoreError` gains the `OpBudget` variant (#9487) and is now `#[non_exhaustive]`. A `match` on it outside trusty-common needs a wildcard arm; later variants are no longer a breaking change.
