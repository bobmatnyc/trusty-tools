Fixed
- `tm fleet init` run as an upgrade no longer replaces a file under the Architect's `local/` (instance guidance, owner profile, config, IL schedule, records) when it still equals an earlier shipped template; framework scripts, schemas and dashboard templates are still refreshed (#10608).
