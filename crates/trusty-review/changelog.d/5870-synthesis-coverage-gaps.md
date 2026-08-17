Added

- The report-synthesis system prompt now asks for a coverage-gaps note. When the
  analysed data references a repository, service, database schema, or project
  board that is not one of the assessed applications, the executive summary names
  it and recommends the operator register it and re-run. Schema and migration
  repositories are called out specifically — a finding citing a table or a
  migration with no schema repository in the assessed set is the common tell. The
  instruction is static, so a template overriding `executive_summary` cannot drop
  it, and it permits naming only what the data actually references — never a
  target the model infers probably exists.
