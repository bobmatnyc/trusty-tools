Fixed
- `palace_resolve::read_project_pin` refuses a pin whose `schema_version` is
  newer than `PIN_SCHEMA_VERSION`, with the new
  `PalaceResolveError::PinSchemaTooNew { path, found, supported }` naming the
  file and both versions. The version is read before the full parse, so a newer
  pin of any shape reports as too new, not malformed (ADR-0067 D2, #9274).
- `ProjectPin` keeps the keys it does not know and writes them back, and
  `ProjectPin::preserving_unknown_fields_of` carries them from the pin on disk
  into a freshly built one, so a rewrite no longer drops a field a later release
  added (#9274).
