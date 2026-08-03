Changed

- `inference::types::SecretString` is documented as superseded by
  `credentials::Secret`. Its behaviour is unchanged; note that its
  four-character head preview does **not** meet DOC-45 `C-8.2`, and collapsing
  the two types is left to a follow-up because it requires changing the existing
  test that pins that preview's shape.

Note: this change adds **no authorization**. `resolve` accepts a `Principal` and
checks no grant against it — grants, the ACL, default-deny and the code-owned
floor are [#4566](https://github.com/bobmatnyc/trusty-tools/issues/4566). The
signature is final so no consumer is migrated twice.
