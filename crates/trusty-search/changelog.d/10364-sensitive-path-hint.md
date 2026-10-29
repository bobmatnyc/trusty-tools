Changed

- The refusal for a path under a temp or app-support prefix (for example `/private/tmp`) now names `--allow-sensitive-path` as the opt-in. Refusals the flag cannot relax (`.ssh`, `.env`, home top-level directories) are unchanged.
