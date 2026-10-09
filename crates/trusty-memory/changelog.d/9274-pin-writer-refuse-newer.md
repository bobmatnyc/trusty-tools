Fixed
- `write_project_pin` re-reads the pin on disk first. It refuses to overwrite a
  pin with a newer `schema_version`, or one it cannot read or parse, and it
  keeps the unknown fields of the pin it replaces, so `trusty-memory link
  --force` no longer drops a field a later release added (ADR-0067 D2, #9274).
