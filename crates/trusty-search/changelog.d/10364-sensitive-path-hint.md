Changed

- `trusty-search index add` now names `--allow-sensitive-path` in its refusal for a path under a temp or app-support prefix (for example `/private/tmp`). Plain `index`, daemon start and restore keep the unchanged refusal, because the flag does not apply there. Refusals the flag cannot relax (`.ssh`, `.env`, home top-level directories) are unchanged.
