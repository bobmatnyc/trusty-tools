Fixed

- `trusty-search index <path> -i <id>` now registers and indexes under `<id>` instead of silently using the path basename. A typed `-i` that differs from `--name` is refused with an error naming both; an env-only `TRUSTY_INDEX` does not rename the registration. In a repo with a `trusty-search.yaml` manifest, `-i <id>` also selects that one declared index, as `--name` does (#12161).
