Fixed
- `okg_register_source` loads, upserts and saves `_sources/registry.toml` under the bundle lock (`SourceRegistry::upsert_at`, SPEC-OKF-01 §4.4), so two concurrent registrations no longer drop each other's row (#10746).
