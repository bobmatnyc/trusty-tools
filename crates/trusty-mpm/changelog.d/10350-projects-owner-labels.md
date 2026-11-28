Fixed

- The seeded Architect dashboard's Projects view labels a project registered by its checkout path as `owner/repo`, taken from the registry record's `project_key`, instead of showing the bare alias. A project whose key is `local:<uuid>`, absent, or in another shape keeps its alias as the label, and no owner is guessed (Refs #10350).
- The Architect dashboard's Visualize view names a project registered by its checkout path `owner/repo` from its `project_key`, as the Projects view does, instead of the bare alias (Refs #10350).
