Added
- `content::resolve` refuses a bundle whose `bundle-manifest.toml` declares a
  `schema_major` newer than `SUPPORTED_SCHEMA_MAJOR` (1) with the new
  `ContentError::UnsupportedSchema`, and refuses a manifest with no
  `schema_major` as `BundleCorrupt` (ADR-0064 PHASE_3 (iv)).
- `content::validate_tag` is public, so a caller can refuse a malformed
  `content-vX.Y.Z` tag before it builds a download URL from it.
