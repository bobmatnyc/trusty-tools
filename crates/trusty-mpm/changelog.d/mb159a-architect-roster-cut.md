Changed

- An Architect launch hides 22 never-dispatched bundled agents (the language, framework, `api-qa`, `data-designer`, `gcp-ops`, `ops`, manager and `prompt-engineer` agents) from its roster, cutting about 4k characters from the session start. The launch's one `--settings` payload carries `Agent(<name>)` and `Task(<name>)` deny rules for them. PM launches and the deployed agent files are unchanged, so a project's PM session can still dispatch any of them.
