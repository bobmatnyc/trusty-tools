Fixed

- The seeded Architect dashboard's Projects view labels a project registered by its checkout path as `owner/repo`, taken from the registry record's `project_key`, instead of showing the bare alias. A project whose key is `local:<uuid>`, absent, or in another shape keeps its alias as the label, and no owner is guessed (Refs #10350).
